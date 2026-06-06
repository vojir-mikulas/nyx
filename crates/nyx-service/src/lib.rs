// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The Nyx backend service.
//!
//! GPUI runs its own executor on the main thread; `russh` (the SFTP transport) is
//! Tokio-based. So the backend lives on a **dedicated thread** that owns a Tokio
//! runtime, the active connection and (later) the transfer queue. The UI talks to
//! it over two channels:
//!
//! - [`Command`] — UI → service (sent synchronously from the GPUI thread over a
//!   Tokio mpsc; a send never blocks the UI).
//! - [`Event`] — service → UI. This side is a `futures::channel::mpsc` so the
//!   GPUI **foreground** executor can `await` it as a `Stream` inside `cx.spawn`
//!   (a blocking `std` recv there would freeze the UI).
//!
//! M2 wires the connect + list vertical slice: [`Command::Connect`] /
//! [`Command::ListDir`] / [`Command::Disconnect`], host-key trust-on-first-use via
//! [`Command::HostKeyDecision`], and the matching events. A single connection is
//! supported (the active session); multi-session is out of scope until later.
//!
//! M3 adds exactly one command/event pair — [`Command::TestConnection`] /
//! [`Event::TestResult`] — for the connection editor's "Test" button. The probe
//! spins up a *transient* client that never touches the stored session. A
//! single-flight guard makes this safe: at most one connect-like op (Connect or
//! TestConnection) is in flight at a time, so there is never more than one
//! pending host-key decision.

use std::fmt;
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use futures::channel::mpsc::{
    unbounded as futures_unbounded, UnboundedReceiver as FuturesReceiver,
    UnboundedSender as FuturesSender,
};
use nyx_core::RemoteEntry;
use nyx_profile::Profile;
use nyx_protocol::{KnownHosts, RemoteClient, SftpClient};
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver as TokioReceiver, UnboundedSender as TokioSender,
};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

/// The per-OS path to the trust-on-first-use `known_hosts` store
/// (`<data_dir>/known_hosts`, resolved via the `directories` crate).
///
/// Falls back to `./known_hosts` only if the OS data dir can't be resolved
/// (never expected in practice).
fn known_hosts() -> PathBuf {
    match directories::ProjectDirs::from("dev", "nyx", "Nyx") {
        Some(dirs) => dirs.data_dir().join("known_hosts"),
        None => {
            warn!("could not resolve the OS data directory; using ./known_hosts");
            PathBuf::from("known_hosts")
        }
    }
}

/// A password that never reveals itself in `Debug`/logs.
///
/// The inner string is only reachable via [`Secret::expose`], which is called in
/// exactly one place (the SFTP auth call). Everything else — including the derived
/// `Debug` on [`Command`] — sees `***`.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wrap a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reveal the secret. Call sites must not log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// A request from the UI to the backend.
#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    /// Connect to `profile`, authenticating with `password`.
    ///
    /// The password is wrapped in [`Secret`] so it can never reach a log.
    Connect {
        /// The profile to connect to.
        profile: Profile,
        /// The login password (redacted in `Debug`).
        password: Secret,
    },
    /// The user's answer to a pending [`Event::HostKeyPrompt`].
    HostKeyDecision {
        /// `true` to trust (and persist) the host key, `false` to abort.
        accept: bool,
    },
    /// List a remote directory on the active connection.
    ListDir {
        /// Absolute remote path to list.
        path: String,
    },
    /// Validate a profile's credentials without opening a browser session.
    ///
    /// Spins up a throwaway client (its own connect + drop), entirely separate
    /// from the stored session, and reports back via [`Event::TestResult`]. The
    /// password is wrapped in [`Secret`] so it can never reach a log.
    TestConnection {
        /// The profile to probe.
        profile: Profile,
        /// The login password (redacted in `Debug`).
        password: Secret,
    },
    /// Close the active connection.
    Disconnect,
    /// Shut the runtime down and exit the thread.
    Shutdown,
}

/// A message from the backend to the UI.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// The backend thread and Tokio runtime are up.
    Ready,
    /// The backend has stopped (after [`Command::Shutdown`] or channel drop).
    Stopped,
    /// A connection attempt has started for `profile_id`.
    Connecting {
        /// The connecting profile's id.
        profile_id: String,
    },
    /// An unknown host key needs the user's trust decision (TOFU).
    ///
    /// The UI shows a prompt and replies with [`Command::HostKeyDecision`].
    HostKeyPrompt {
        /// The host the key belongs to.
        host: String,
        /// The SHA-256 fingerprint, e.g. `SHA256:…`.
        fingerprint: String,
    },
    /// The active connection is established for `profile_id`.
    Connected {
        /// The connected profile's id.
        profile_id: String,
    },
    /// A directory listing for `path` on the active connection.
    DirListing {
        /// The path that was listed (echoed so the UI can drop stale listings).
        path: String,
        /// The entries in that directory.
        entries: Vec<RemoteEntry>,
    },
    /// The outcome of a [`Command::TestConnection`] probe, matched by `profile_id`.
    ///
    /// The `message` is human-readable and credential-free (e.g. `"Connection
    /// OK"` or an error detail).
    TestResult {
        /// The probed profile's id (so the editor can match its inline status).
        profile_id: String,
        /// Whether the probe succeeded.
        ok: bool,
        /// A credential-free status / error message.
        message: String,
    },
    /// An operation failed. The message is human-readable and credential-free.
    Error {
        /// The error detail (a `NyxError` display; never contains a secret).
        message: String,
    },
}

/// Handle to the running backend thread.
///
/// Send [`Command`]s with [`ServiceHandle::send`]. Dropping the handle requests
/// shutdown and joins the thread.
pub struct ServiceHandle {
    commands: TokioSender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl ServiceHandle {
    /// Send a command to the backend. Returns `false` if the service has gone.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Spawn the backend thread and its Tokio runtime.
///
/// Returns the [`ServiceHandle`] (UI → service) and the [`Event`] receiver
/// (service → UI) the UI drains as a `Stream` on the GPUI foreground executor.
pub fn spawn() -> (ServiceHandle, FuturesReceiver<Event>) {
    let (cmd_tx, cmd_rx) = unbounded_channel::<Command>();
    let (evt_tx, evt_rx) = futures_unbounded::<Event>();

    let thread = thread::Builder::new()
        .name("nyx-service".into())
        .spawn(move || run(cmd_rx, evt_tx))
        .expect("failed to spawn nyx-service thread");

    (
        ServiceHandle {
            commands: cmd_tx,
            thread: Some(thread),
        },
        evt_rx,
    )
}

/// The backend thread entry point: build the runtime and drive the dispatcher.
fn run(commands: TokioReceiver<Command>, events: FuturesSender<Event>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("nyx-service-worker")
        .build()
        .expect("failed to build Tokio runtime");

    let _ = events.unbounded_send(Event::Ready);
    runtime.block_on(dispatch(commands, events.clone()));
    let _ = events.unbounded_send(Event::Stopped);
}

/// Whether a connect-like task is a live connect or a throwaway test probe.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskKind {
    /// A live `Connect` — its session is kept on success.
    Connect,
    /// A `TestConnection` probe — the client is dropped after reporting.
    Test,
}

/// The result of a connect-like task, handed back to the dispatcher.
enum TaskOutcome {
    /// A live connect succeeded — the dispatcher takes ownership of the session.
    Connected {
        profile_id: String,
        client: Box<SftpClient>,
    },
    /// A live connect failed with a credential-free message.
    ConnectFailed { message: String },
    /// A test probe finished (success or failure).
    TestResult {
        profile_id: String,
        ok: bool,
        message: String,
    },
}

/// The single command dispatcher. Owns the active session and the host-key
/// decision slot; connect-like ops run as spawned tasks so the loop stays
/// responsive to [`Command::HostKeyDecision`] while a handshake awaits the user.
///
/// A single-flight guard (`in_flight`) covers both `Connect` and
/// `TestConnection`: while one is running, a second connect-like command is
/// rejected outright, so the single host-key slot can never be contended.
async fn dispatch(mut commands: TokioReceiver<Command>, events: FuturesSender<Event>) {
    let mut client: Option<Box<SftpClient>> = None;
    // The responder for an in-flight host-key prompt. With the single-flight
    // guard there is at most one connect-like op, hence at most one slot user.
    let mut pending_host_key: Option<oneshot::Sender<bool>> = None;
    // Whether a connect-like op (Connect or TestConnection) is in flight.
    let mut in_flight = false;

    // Internal channels: connect-like task → dispatcher.
    let (register_tx, mut register_rx) = unbounded_channel::<oneshot::Sender<bool>>();
    let (done_tx, mut done_rx) = unbounded_channel::<TaskOutcome>();

    loop {
        tokio::select! {
            maybe_cmd = commands.recv() => {
                let Some(command) = maybe_cmd else { break };
                match command {
                    Command::Shutdown => break,
                    Command::Connect { profile, password } => {
                        if in_flight {
                            let _ = events.unbounded_send(Event::Error {
                                message: "a connection is already in progress".into(),
                            });
                            continue;
                        }
                        // Replace any existing session.
                        client = None;
                        in_flight = true;
                        tokio::spawn(run_task(
                            TaskKind::Connect,
                            profile,
                            password,
                            events.clone(),
                            register_tx.clone(),
                            done_tx.clone(),
                        ));
                    }
                    Command::TestConnection { profile, password } => {
                        if in_flight {
                            let _ = events.unbounded_send(Event::TestResult {
                                profile_id: profile.id,
                                ok: false,
                                message: "a connection is already in progress".into(),
                            });
                            continue;
                        }
                        in_flight = true;
                        tokio::spawn(run_task(
                            TaskKind::Test,
                            profile,
                            password,
                            events.clone(),
                            register_tx.clone(),
                            done_tx.clone(),
                        ));
                    }
                    Command::HostKeyDecision { accept } => {
                        if let Some(responder) = pending_host_key.take() {
                            let _ = responder.send(accept);
                        } else {
                            warn!("host-key decision with no pending prompt");
                        }
                    }
                    Command::ListDir { path } => {
                        match client.as_deref() {
                            Some(session) => match session.list_dir(&path).await {
                                Ok(entries) => {
                                    debug!(%path, count = entries.len(), "listed directory");
                                    let _ = events.unbounded_send(Event::DirListing { path, entries });
                                }
                                Err(err) => {
                                    let _ = events.unbounded_send(Event::Error {
                                        message: err.to_string(),
                                    });
                                }
                            },
                            None => {
                                let _ = events.unbounded_send(Event::Error {
                                    message: "not connected".into(),
                                });
                            }
                        }
                    }
                    Command::Disconnect => {
                        // A disconnect also clears the single-flight slot.
                        in_flight = false;
                        if let Some(mut session) = client.take() {
                            let _ = session.disconnect().await;
                            info!("disconnected");
                        }
                    }
                }
            }
            Some(responder) = register_rx.recv() => {
                pending_host_key = Some(responder);
            }
            Some(done) = done_rx.recv() => {
                // Any terminal outcome clears the single-flight slot.
                in_flight = false;
                match done {
                    TaskOutcome::Connected { profile_id, client: session } => {
                        client = Some(session);
                        info!(%profile_id, "connected");
                        let _ = events.unbounded_send(Event::Connected { profile_id });
                    }
                    TaskOutcome::ConnectFailed { message } => {
                        let _ = events.unbounded_send(Event::Error { message });
                    }
                    TaskOutcome::TestResult { profile_id, ok, message } => {
                        let _ = events.unbounded_send(Event::TestResult { profile_id, ok, message });
                    }
                }
            }
        }
    }
}

/// Run a single connect-like attempt and report the outcome to the dispatcher.
///
/// For [`TaskKind::Connect`] a success hands the live session back; for
/// [`TaskKind::Test`] the client is dropped and only a credential-free
/// [`TaskOutcome::TestResult`] is reported (no `Connecting` event, so the test
/// never disturbs the UI's connection state).
async fn run_task(
    kind: TaskKind,
    profile: Profile,
    password: Secret,
    events: FuturesSender<Event>,
    register: TokioSender<oneshot::Sender<bool>>,
    done: TokioSender<TaskOutcome>,
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
    let mut client = SftpClient::new(
        profile.host.clone(),
        profile.port,
        profile.username.clone(),
        password.expose().to_string(),
        KnownHosts::at(known_hosts()),
        prompt,
    );

    let outcome = match (kind, client.connect().await) {
        (TaskKind::Connect, Ok(())) => TaskOutcome::Connected {
            profile_id,
            client: Box::new(client),
        },
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
    let _ = done.send(outcome);
}

/// The service-side host-key prompt: surface a [`Event::HostKeyPrompt`] to the UI
/// and await the user's [`Command::HostKeyDecision`].
mod host_key {
    use super::*;
    use nyx_protocol::HostKeyPrompt;

    /// Bridges the protocol layer's host-key callback to the UI event/command flow.
    pub struct PromptBridge {
        pub events: FuturesSender<Event>,
        pub register: TokioSender<oneshot::Sender<bool>>,
    }

    #[async_trait::async_trait]
    impl HostKeyPrompt for PromptBridge {
        async fn confirm_unknown(&self, host: &str, fingerprint: &str) -> bool {
            let (responder, answer) = oneshot::channel();
            // Register the responder with the dispatcher *before* prompting, so a
            // decision can never arrive with no slot to resolve.
            if self.register.send(responder).is_err() {
                return false;
            }
            let _ = self.events.unbounded_send(Event::HostKeyPrompt {
                host: host.to_string(),
                fingerprint: fingerprint.to_string(),
            });
            // A dropped sender (e.g. shutdown) resolves to "do not trust".
            answer.await.unwrap_or(false)
        }
    }
}
