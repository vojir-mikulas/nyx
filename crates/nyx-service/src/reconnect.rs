//! Connect + auto-reconnect: the connect-like task the dispatcher spawns, the
//! backoff `Reconnector`, client construction, and the host-key prompt bridge.
//!
//! Pure mechanical move out of `lib.rs` (code review 2026-06-08, plan 05).

use super::*;

/// Run a single connect-like attempt and report the outcome to the dispatcher.
///
/// For [`TaskKind::Connect`] a success hands the live session back; for
/// [`TaskKind::Test`] the client is dropped and only a credential-free
/// [`TaskOutcome::TestResult`] is reported (no `Connecting` event, so the test
/// never disturbs the UI's connection state).
pub(crate) async fn run_task(
    kind: TaskKind,
    profile: Profile,
    secret: Secret,
    events: FuturesSender<Event>,
    register: TokioSender<oneshot::Sender<bool>>,
    done: TokioSender<(u64, TaskOutcome)>,
    seq: u64,
) {
    let profile_id = profile.id.clone();
    info!(host = %profile.host, port = profile.port, test = kind == TaskKind::Test, "connecting");
    if kind == TaskKind::Connect {
        let _ = events.unbounded_send(Event::Connecting {
            profile_id: profile_id.clone(),
        });
    }

    let prompt = std::sync::Arc::new(host_key::PromptBridge {
        events: events.clone(),
        register,
    });
    // Build the protocol client from the profile + carried secret. A profile-level
    // rejection (e.g. key auth on FTP) is reported here without ever connecting.
    let mut client = match build_client(&profile, secret, prompt) {
        Ok(client) => client,
        Err(err) => {
            let _ = done.send((seq, connect_error_outcome(kind, profile_id, err)));
            return;
        }
    };

    let outcome = match (kind, client.connect().await) {
        (TaskKind::Connect, Ok(())) => {
            // Resolve the landing directory once, up front; fall back to root if
            // the server doesn't answer `canonicalize`.
            let home = client
                .default_dir()
                .await
                .unwrap_or_else(|_| RemotePath::root());
            TaskOutcome::Connected {
                profile_id,
                protocol: profile.protocol,
                client,
                home,
            }
        }
        (TaskKind::Connect, Err(err)) => TaskOutcome::ConnectFailed {
            message: err.to_string(),
        },
        (TaskKind::Test, Ok(())) => {
            // The transient client is dropped here (its `Drop` closes the
            // connection), never touching the stored session.
            let _ = client.disconnect().await;
            TaskOutcome::TestResult {
                profile_id,
                ok: true,
                message: "Connection OK".into(),
            }
        }
        (TaskKind::Test, Err(err)) => TaskOutcome::TestResult {
            profile_id,
            ok: false,
            message: err.to_string(),
        },
    };
    let _ = done.send((seq, outcome));
}

/// Credentials cached for a live session's lifetime so an automatic reconnect
/// needs no UI round-trip per attempt. Held only while the session is alive and
/// dropped - zeroizing the [`Secret`] - on disconnect or when reconnect gives up.
pub(crate) struct SessionCreds {
    pub(crate) profile: Profile,
    secret: Secret,
    auto_reconnect: bool,
}

/// Owns the auto-reconnect state: the cached session credentials and the running
/// backoff task. The connection-loss path asks it to [`start`](Self::start) a
/// self-contained reconnect loop; the command loop can [`abort`](Self::abort) or
/// [`clear`](Self::clear) it.
pub(crate) struct Reconnector {
    pub(crate) creds: Option<SessionCreds>,
    task: Option<tokio::task::JoinHandle<()>>,
    register: TokioSender<oneshot::Sender<bool>>,
    done: TokioSender<(u64, TaskOutcome)>,
    /// A monotonic epoch over connect-like *attempts* (manual Connect, the probe
    /// `Test`, and the auto-reconnect loop) - the value each spawned attempt is
    /// stamped with, so [`dispatch`] can drop a straggler whose epoch it has since
    /// bumped (a reconnect `Connected` already queued before [`abort`](Self::abort),
    /// which abort can't un-send). Bumped on a *superseding session intent*
    /// (Connect / Disconnect / CancelReconnect) but **not** on a `Test` probe or a
    /// reconnect-loop start, neither of which supersedes the current intent.
    /// Distinct from `generation`, which guards in-flight *transfers*.
    seq: u64,
}

impl Reconnector {
    pub(crate) fn new(
        register: TokioSender<oneshot::Sender<bool>>,
        done: TokioSender<(u64, TaskOutcome)>,
    ) -> Self {
        Self {
            creds: None,
            task: None,
            register,
            done,
            seq: 0,
        }
    }

    /// The current connect epoch (for the dispatcher's staleness check).
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    /// Bump and return the epoch: a new superseding connect intent. Any outcome
    /// still carrying an earlier epoch is now stale and dropped by the dispatcher.
    pub(crate) fn bump(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// Cache the credentials for the session being (re)connected.
    pub(crate) fn set_creds(&mut self, profile: Profile, secret: Secret, auto_reconnect: bool) {
        self.creds = Some(SessionCreds {
            profile,
            secret,
            auto_reconnect,
        });
    }

    /// Abort the running backoff loop, if any (keeps the cached credentials).
    pub(crate) fn abort(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    /// Abort the loop and drop the cached credentials (zeroizing the secret).
    pub(crate) fn clear(&mut self) {
        self.abort();
        self.creds = None;
    }

    /// Start a backoff reconnect loop for the lost session - but only when
    /// auto-reconnect is enabled and credentials are cached. A no-op otherwise,
    /// leaving the session lost for a manual reconnect.
    pub(crate) fn start(&mut self, events: &FuturesSender<Event>) {
        self.abort();
        let Some(creds) = self.creds.as_ref() else {
            return;
        };
        if !creds.auto_reconnect {
            return;
        }
        // The loop carries the *current* epoch (a loss recovers the same session,
        // it is not a new intent); a later supersede bumps past it and drops its
        // outcome.
        let task = tokio::spawn(run_reconnect(
            creds.profile.clone(),
            creds.secret.clone(),
            events.clone(),
            self.register.clone(),
            self.done.clone(),
            self.seq,
        ));
        self.task = Some(task);
    }
}

/// Drive the auto-reconnect backoff loop for a lost session.
///
/// Each attempt emits [`Event::Reconnecting`], waits the backoff delay, then dials
/// the profile. A success hands the live session back via [`TaskOutcome::Connected`],
/// the same path a manual connect uses. A *transport* failure is retried; an
/// auth / host-key / locked-key failure is terminal (retrying bad credentials is
/// pointless and can lock accounts). Exhausting the attempts ends in
/// [`TaskOutcome::ReconnectFailed`].
pub(crate) async fn run_reconnect(
    profile: Profile,
    secret: Secret,
    events: FuturesSender<Event>,
    register: TokioSender<oneshot::Sender<bool>>,
    done: TokioSender<(u64, TaskOutcome)>,
    seq: u64,
) {
    let profile_id = profile.id.clone();
    for attempt in 1..=RECONNECT_MAX_ATTEMPTS {
        let delay = backoff_delay(attempt);
        let _ = events.unbounded_send(Event::Reconnecting {
            profile_id: profile_id.clone(),
            attempt,
            next_in: delay,
        });
        tokio::time::sleep(delay).await;

        let prompt = Arc::new(host_key::PromptBridge {
            events: events.clone(),
            register: register.clone(),
        });
        // A construction error (e.g. a misconfigured key) will not heal on retry.
        let mut client = match build_client(&profile, secret.clone(), prompt) {
            Ok(client) => client,
            Err(err) => {
                let _ = done.send((
                    seq,
                    TaskOutcome::ReconnectFailed {
                        profile_id,
                        reason: err.to_string(),
                    },
                ));
                return;
            }
        };
        match client.connect().await {
            Ok(()) => {
                let home = client
                    .default_dir()
                    .await
                    .unwrap_or_else(|_| RemotePath::root());
                let _ = done.send((
                    seq,
                    TaskOutcome::Connected {
                        profile_id,
                        protocol: profile.protocol,
                        client,
                        home,
                    },
                ));
                return;
            }
            Err(err) if is_transient_connect_error(&err) => {
                warn!(%profile_id, attempt, "auto-reconnect attempt failed; will retry");
            }
            Err(err) => {
                let _ = done.send((
                    seq,
                    TaskOutcome::ReconnectFailed {
                        profile_id,
                        reason: err.to_string(),
                    },
                ));
                return;
            }
        }
    }
    let _ = done.send((
        seq,
        TaskOutcome::ReconnectFailed {
            profile_id,
            reason: "could not reconnect after several attempts".into(),
        },
    ));
}

/// Whether a failed connect attempt is worth retrying: a transport / network
/// failure (the server may still be down) is, but an auth, host-key or locked-key
/// rejection is not - see [`run_reconnect`].
pub(crate) fn is_transient_connect_error(err: &NyxError) -> bool {
    matches!(
        err,
        NyxError::Connection(_) | NyxError::ConnectionLost(_) | NyxError::Io(_)
    )
}

/// The backoff delay before attempt `n` (1-based): an exponential base (1s, 2s,
/// 4s … capped at [`RECONNECT_CAP`]) plus up to 50% jitter, so a flapping link or
/// many clients retrying don't hammer the server in lockstep.
pub(crate) fn backoff_delay(attempt: u32) -> Duration {
    let base = RECONNECT_CAP.min(Duration::from_secs(
        1u64 << attempt.saturating_sub(1).min(5),
    ));
    let base_ms = base.as_millis() as u64;
    Duration::from_millis(base_ms + jitter_ms(base_ms / 2))
}

/// A cheap, non-cryptographic jitter in `0..max_ms`, seeded from the wall clock -
/// used only to desynchronize backoff, so randomness quality is irrelevant.
pub(crate) fn jitter_ms(max_ms: u64) -> u64 {
    if max_ms == 0 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % max_ms
}

/// Build the protocol client for a profile, keyed on `profile.protocol`.
///
/// This is the one construction seam: everything downstream speaks
/// [`RemoteClient`]. Key auth is SFTP-only, so an FTP/FTPS profile that selects it
/// is rejected here with a clear message rather than silently ignored.
pub(crate) fn build_client(
    profile: &Profile,
    secret: Secret,
    prompt: Arc<host_key::PromptBridge>,
) -> Result<Box<dyn RemoteClient>, NyxError> {
    match profile.protocol {
        Protocol::Sftp => {
            // For key auth an empty secret means "unencrypted key" (no passphrase).
            let auth = match &profile.auth {
                AuthMethod::Password => Auth::Password(secret.expose().to_string()),
                AuthMethod::Key { path } => {
                    let passphrase = secret.expose();
                    Auth::Key {
                        path: path.clone(),
                        passphrase: (!passphrase.is_empty()).then(|| passphrase.to_string()),
                    }
                }
                AuthMethod::Anonymous => {
                    return Err(NyxError::Other(
                        "anonymous login is only supported for FTP/FTPS".into(),
                    ));
                }
            };
            Ok(Box::new(SftpClient::new(
                profile.host.clone(),
                profile.port,
                profile.username.clone(),
                auth,
                KnownHosts::at(known_hosts()),
                prompt,
            )))
        }
        Protocol::Ftp => {
            reject_key_auth(profile)?;
            let (username, password) = ftp_credentials(profile, &secret);
            Ok(Box::new(FtpClient::new(
                profile.host.clone(),
                profile.port,
                username,
                password,
            )))
        }
        Protocol::Ftps => {
            reject_key_auth(profile)?;
            let (username, password) = ftp_credentials(profile, &secret);
            Ok(Box::new(FtpsClient::new(
                profile.host.clone(),
                profile.port,
                username,
                password,
                profile.ftps_mode,
                KnownHosts::at(known_certs()),
                prompt,
            )))
        }
    }
}

/// Historical anonymous-FTP password convention; sent as `PASS` for an anonymous
/// login. Empty passwords are rejected by some servers, so use the standard token.
pub(crate) const ANON_PASSWORD: &str = "anonymous@";

/// Resolve the `(username, password)` an FTP/FTPS login should send. Anonymous
/// ignores the stored username and any secret; otherwise the profile username and
/// the exposed secret are used.
pub(crate) fn ftp_credentials(profile: &Profile, secret: &Secret) -> (String, String) {
    match profile.auth {
        AuthMethod::Anonymous => ("anonymous".to_string(), ANON_PASSWORD.to_string()),
        _ => (profile.username.clone(), secret.expose().to_string()),
    }
}

/// Reject key auth for a non-SFTP protocol (FTP/FTPS are username+password only).
pub(crate) fn reject_key_auth(profile: &Profile) -> Result<(), NyxError> {
    if matches!(profile.auth, AuthMethod::Key { .. }) {
        return Err(NyxError::Other(
            "key authentication is only supported for SFTP".into(),
        ));
    }
    Ok(())
}

/// Map a pre-connect build error to the right terminal outcome for the task kind.
pub(crate) fn connect_error_outcome(
    kind: TaskKind,
    profile_id: String,
    err: NyxError,
) -> TaskOutcome {
    match kind {
        TaskKind::Connect => TaskOutcome::ConnectFailed {
            message: err.to_string(),
        },
        TaskKind::Test => TaskOutcome::TestResult {
            profile_id,
            ok: false,
            message: err.to_string(),
        },
    }
}

/// The service-side trust prompt: surface a [`Event::HostKeyPrompt`] to the UI
/// and await the user's [`Command::HostKeyDecision`]. Serves both SSH host keys
/// (SFTP) and TLS certificates (FTPS) - the single decision slot is safe because
/// the single-flight guard allows only one connect-like op at a time.
pub(crate) mod host_key {
    use super::*;
    use nyx_protocol::ServerTrustPrompt;

    /// Bridges the protocol layer's trust callback to the UI event/command flow.
    pub struct PromptBridge {
        pub events: FuturesSender<Event>,
        pub register: TokioSender<oneshot::Sender<bool>>,
    }

    #[async_trait::async_trait]
    impl ServerTrustPrompt for PromptBridge {
        async fn confirm_unknown(
            &self,
            host: &str,
            fingerprint: &str,
            kind: ServerTrustKind,
        ) -> bool {
            let (responder, answer) = oneshot::channel();
            // Register the responder with the dispatcher *before* prompting, so a
            // decision can never arrive with no slot to resolve.
            if self.register.send(responder).is_err() {
                return false;
            }
            let _ = self.events.unbounded_send(Event::HostKeyPrompt {
                host: host.to_string(),
                fingerprint: fingerprint.to_string(),
                kind,
            });
            // A dropped sender (e.g. shutdown) resolves to "do not trust".
            answer.await.unwrap_or(false)
        }
    }
}
