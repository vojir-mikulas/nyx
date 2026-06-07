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
//! A single connection is supported (the active session); multi-session is out
//! of scope. [`Command::TestConnection`] / [`Event::TestResult`] back the
//! connection editor's "Test" button: the probe spins up a *transient* client
//! that never touches the stored session. A single-flight guard makes this safe:
//! at most one connect-like op (Connect or TestConnection) is in flight at a
//! time, so there is never more than one pending host-key decision.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures::channel::mpsc::{
    unbounded as futures_unbounded, UnboundedReceiver as FuturesReceiver,
    UnboundedSender as FuturesSender,
};
use nyx_core::{
    CollisionChoice, EntryKind, NyxError, Protocol, RemoteEntry, RemotePath, Secret,
    ServerTrustKind, TransferDirection, TransferId, TransferKind, TransferStatus,
};
use nyx_profile::{AuthMethod, Profile};
use nyx_protocol::{
    Auth, DirWalk, FtpClient, FtpsClient, KnownHosts, RemoteClient, SftpClient, WalkItem,
};
use nyx_transfer::{CancelOutcome, Started, TransferQueue, TransferSpec};
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver as TokioReceiver, UnboundedSender as TokioSender,
};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

/// The global concurrency cap: at most this many transfers run at once;
/// submissions past it wait in the queue. Per-profile / settings caps are
/// post-MVP.
const MAX_CONCURRENT_TRANSFERS: usize = 3;

/// How often the dispatcher samples running transfers' byte counters to emit a
/// throttled [`Event::TransferProgress`]. The fixed interval also serves as the
/// speed denominator, so no `Instant` is needed.
const PROGRESS_TICK: Duration = Duration::from_millis(150);

/// How many automatic reconnect attempts to make on a transport loss before
/// giving up and falling back to a manual reconnect.
const RECONNECT_MAX_ATTEMPTS: u32 = 6;

/// The exponential-backoff cap: the base wait doubles per attempt (1s, 2s, 4s …)
/// but never exceeds this, with jitter added on top to avoid reconnect storms.
const RECONNECT_CAP: Duration = Duration::from_secs(30);

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

/// The per-OS path to the FTPS trust-on-first-use `known_certs` store
/// (`<data_dir>/known_certs`) — the certificate parallel to [`known_hosts`].
fn known_certs() -> PathBuf {
    match directories::ProjectDirs::from("dev", "nyx", "Nyx") {
        Some(dirs) => dirs.data_dir().join("known_certs"),
        None => {
            warn!("could not resolve the OS data directory; using ./known_certs");
            PathBuf::from("known_certs")
        }
    }
}

/// A request from the UI to the backend.
#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    /// Connect to `profile`, authenticating with `secret`.
    ///
    /// `secret` is the password or — for key auth — the key passphrase (empty for
    /// an unencrypted key); the method itself comes from `profile.auth`. It is
    /// wrapped in [`Secret`] so it can never reach a log.
    Connect {
        /// The profile to connect to.
        profile: Profile,
        /// The password or key passphrase (redacted in `Debug`).
        secret: Secret,
        /// Whether to auto-reconnect (with backoff) if this session's transport
        /// later drops. The profile + secret are cached for the session's
        /// lifetime so backoff needs no UI round-trip.
        auto_reconnect: bool,
    },
    /// The user's answer to a pending [`Event::HostKeyPrompt`].
    HostKeyDecision {
        /// `true` to trust (and persist) the host key, `false` to abort.
        accept: bool,
    },
    /// List a remote directory on the active connection.
    ListDir {
        /// Absolute remote path to list.
        path: RemotePath,
    },
    /// Resolve a symlink on the active connection by following it, so the UI can
    /// decide on click whether to navigate into it (directory target) or treat it
    /// as a file (download). Replies with [`Event::SymlinkResolved`].
    ResolveSymlink {
        /// Absolute path of the symlink to follow.
        path: RemotePath,
    },
    /// Create a remote directory on the active connection.
    Mkdir {
        /// Absolute remote path of the new directory.
        path: RemotePath,
    },
    /// Rename / move a remote entry on the active connection.
    Rename {
        /// Current absolute remote path.
        from: RemotePath,
        /// New absolute remote path.
        to: RemotePath,
    },
    /// Delete a remote entry on the active connection.
    ///
    /// `is_dir` lets the protocol pick a file delete vs. a recursive directory
    /// delete without an extra stat round-trip on the UI's behalf.
    Remove {
        /// Absolute remote path to delete.
        path: RemotePath,
        /// Whether the target is a directory (recursive delete).
        is_dir: bool,
    },
    /// Download a remote file (or whole directory) to a chosen local path.
    Download {
        /// Absolute remote path to read.
        remote: RemotePath,
        /// Local destination chosen by the user.
        local: PathBuf,
        /// Whether the remote path is a directory (recursive download).
        is_dir: bool,
    },
    /// Upload a local file (or whole directory) to a remote path in the active
    /// connection's cwd.
    Upload {
        /// Local source path chosen by the user.
        local: PathBuf,
        /// Absolute remote destination path.
        remote: RemotePath,
        /// Whether the local path is a directory (recursive upload).
        is_dir: bool,
    },
    /// Validate a profile's credentials without opening a browser session.
    ///
    /// Spins up a throwaway client (its own connect + drop), entirely separate
    /// from the stored session, and reports back via [`Event::TestResult`]. The
    /// secret is wrapped in [`Secret`] so it can never reach a log.
    TestConnection {
        /// The profile to probe.
        profile: Profile,
        /// The password or key passphrase (redacted in `Debug`).
        secret: Secret,
    },
    /// Cancel a queued or running transfer by id.
    ///
    /// A queued transfer is dropped before it starts; a running one is stopped
    /// mid-flight between chunks. Either way the UI receives a terminal
    /// [`Event::TransferDone`] with `Cancelled`.
    CancelTransfer {
        /// The transfer to cancel.
        id: TransferId,
    },
    /// The user's answer to a pending [`Event::TransferCollision`].
    ///
    /// `Overwrite` resumes the transfer (truncating the destination); `Skip`
    /// leaves the existing file and ends the transfer `Skipped`; `Cancel` aborts
    /// it. `apply_to_all` stamps the same choice onto every other pending
    /// collision and still-queued transfer so only one prompt is shown.
    ResolveCollision {
        /// The parked transfer being resolved.
        id: TransferId,
        /// The chosen resolution.
        choice: CollisionChoice,
        /// Apply this choice to all other pending/queued transfers too.
        apply_to_all: bool,
    },
    /// Stop an in-progress auto-reconnect backoff loop, leaving the session lost
    /// (the user can still reconnect manually). A no-op if none is running.
    CancelReconnect,
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
    /// An unknown server identity (SSH host key or TLS certificate) needs the
    /// user's trust decision (TOFU).
    ///
    /// The UI shows a prompt and replies with [`Command::HostKeyDecision`]. `kind`
    /// lets the UI word it correctly per protocol.
    HostKeyPrompt {
        /// The host the identity belongs to.
        host: String,
        /// The SHA-256 fingerprint (`SHA256:…` for a host key or certificate).
        fingerprint: String,
        /// Whether this is an SSH host key (SFTP) or a TLS certificate (FTPS).
        kind: ServerTrustKind,
    },
    /// The active connection is established for `profile_id`.
    Connected {
        /// The connected profile's id.
        profile_id: String,
        /// The resolved default landing directory (home), used when the profile
        /// has no explicit remote path.
        home: RemotePath,
    },
    /// A directory listing for `path` on the active connection.
    DirListing {
        /// The path that was listed (echoed so the UI can drop stale listings).
        path: RemotePath,
        /// The entries in that directory.
        entries: Vec<RemoteEntry>,
    },
    /// The result of a [`Command::ResolveSymlink`]: the followed link's target is
    /// a directory (navigate into `path`) or not (treat `path` as a file).
    SymlinkResolved {
        /// The symlink path that was followed (paths are not secrets).
        path: RemotePath,
        /// Whether the link's target is a directory.
        is_dir: bool,
    },
    /// The active connection's transport died mid-session (network drop, VPN flap,
    /// server restart, sleep/wake). The session is now flipped to "lost": further
    /// commands fail fast until the UI reconnects. Credential-free.
    ConnectionLost {
        /// The profile whose connection was lost.
        profile_id: String,
        /// A human-readable, credential-free reason.
        reason: String,
    },
    /// An automatic reconnect attempt is underway after a transport loss. Emitted
    /// once per attempt, before the backoff wait; [`Event::Connected`] is the
    /// success terminal, [`Event::ReconnectFailed`] the give-up terminal.
    Reconnecting {
        /// The profile being reconnected.
        profile_id: String,
        /// The 1-based attempt number.
        attempt: u32,
        /// How long the backoff waits before this attempt actually dials.
        next_in: Duration,
    },
    /// Auto-reconnect gave up (attempts exhausted, or a non-transport failure such
    /// as a changed host key). The session stays lost; only a manual reconnect
    /// remains. Credential-free.
    ReconnectFailed {
        /// The profile that could not be reconnected.
        profile_id: String,
        /// A human-readable, credential-free reason.
        reason: String,
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
    /// A file operation completed successfully on the active connection.
    ///
    /// `op` tells the UI whether to refresh the current listing (mutating ops do,
    /// a download does not); `message` is a ready-to-toast success line.
    FileOpDone {
        /// Which operation completed (drives the refresh decision).
        op: FileOp,
        /// A credential-free, human-readable success message.
        message: String,
    },
    /// A transfer was accepted into the queue. The UI creates a `Queued` dock
    /// row; paths are not secrets, so they are safe to carry.
    TransferQueued {
        /// The assigned transfer id.
        id: TransferId,
        /// Upload or download.
        direction: TransferDirection,
        /// File or whole-directory transfer.
        kind: TransferKind,
        /// The remote-side path.
        remote: RemotePath,
        /// The local-side path (display form).
        local: String,
    },
    /// A transfer's destination already exists; the pre-flight gate parked it
    /// pending the user's [`Command::ResolveCollision`]. Paths are not secrets.
    TransferCollision {
        /// The parked transfer's id.
        id: TransferId,
        /// Upload or download (which side the existing destination is on).
        direction: TransferDirection,
        /// Whether the existing destination is a directory (folder merge prompt).
        is_dir: bool,
        /// The remote-side path.
        remote: RemotePath,
        /// The local-side path (display form).
        local: String,
        /// Size of the existing destination, if it could be statted (always
        /// `None` for a directory).
        existing_size: Option<u64>,
    },
    /// A transfer left the queue and is now running. `total` is the size statted
    /// at start (`None` if it could not be determined).
    TransferStarted {
        /// The transfer id.
        id: TransferId,
        /// Total size in bytes, if known.
        total: Option<u64>,
    },
    /// A throttled progress sample for a running transfer (~every 150 ms).
    TransferProgress {
        /// The transfer id.
        id: TransferId,
        /// Cumulative bytes transferred so far.
        transferred: u64,
        /// Instantaneous speed in bytes/sec over the last sample interval.
        speed_bps: u64,
    },
    /// A transfer reached a terminal state: `Completed`, `Failed`, `Cancelled`
    /// or `Skipped`. `message` carries the credential-free error detail for
    /// `Failed`.
    TransferDone {
        /// The transfer id.
        id: TransferId,
        /// The terminal status.
        status: TransferStatus,
        /// An error detail for `Failed`; `None` otherwise.
        message: Option<String>,
    },
    /// An operation failed. The message is human-readable and credential-free.
    Error {
        /// The error detail (a `NyxError` display; never contains a secret).
        message: String,
    },
}

/// Which file operation a [`Event::FileOpDone`] refers to.
///
/// The UI's only per-op divergence is whether to refresh the current listing:
/// the mutating ops do; `Download` leaves the remote unchanged and only toasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOp {
    /// A directory was created.
    Mkdir,
    /// An entry was renamed / moved.
    Rename,
    /// An entry (file or recursive directory) was deleted.
    Remove,
    /// A local file was uploaded to the remote.
    Upload,
    /// A remote file was downloaded to disk.
    Download,
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

    /// A cloneable, `Send + Sync` sender for the command channel.
    ///
    /// Lets off-thread code (e.g. an OS drag-promise callback) submit commands
    /// without borrowing the UI-thread [`ServiceHandle`]. It is the same channel
    /// [`send`](Self::send) uses, so ordering and shutdown semantics are shared.
    pub fn commands(&self) -> CommandSender {
        CommandSender {
            tx: self.commands.clone(),
        }
    }
}

/// A detached, thread-safe handle to the command channel (see
/// [`ServiceHandle::commands`]).
#[derive(Clone)]
pub struct CommandSender {
    tx: TokioSender<Command>,
}

impl CommandSender {
    /// Send a command to the backend. Returns `false` if the service has gone.
    pub fn send(&self, command: Command) -> bool {
        self.tx.send(command).is_ok()
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
        client: Box<dyn RemoteClient>,
        /// The resolved default landing directory (home).
        home: RemotePath,
    },
    /// A live connect failed with a credential-free message.
    ConnectFailed { message: String },
    /// An auto-reconnect loop gave up after exhausting its attempts or hitting a
    /// non-transport failure.
    ReconnectFailed { profile_id: String, reason: String },
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
    // `Arc`-shared so slow ops (download/upload/remove) can clone a handle into a
    // detached task and run concurrently against the one session, without
    // blocking the command loop.
    let mut client: Option<Arc<dyn RemoteClient>> = None;
    // The id of the profile behind the active session, for the `ConnectionLost`
    // event. Set on connect, cleared on disconnect or a detected loss.
    let mut active_profile: Option<String> = None;
    // The responder for an in-flight host-key prompt. With the single-flight
    // guard there is at most one connect-like op, hence at most one slot user.
    let mut pending_host_key: Option<oneshot::Sender<bool>> = None;
    // Whether a connect-like op (Connect or TestConnection) is in flight.
    let mut in_flight = false;

    // The transfer scheduler (sans-IO policy) and the per-id byte counters from
    // the previous progress tick (for the speed delta).
    let mut queue = TransferQueue::new(MAX_CONCURRENT_TRANSFERS);
    let mut last_bytes: HashMap<TransferId, u64> = HashMap::new();

    // Internal channels: connect-like task → dispatcher.
    let (register_tx, mut register_rx) = unbounded_channel::<oneshot::Sender<bool>>();
    let (done_tx, mut done_rx) = unbounded_channel::<TaskOutcome>();
    // Internal channel: a finished copy task → dispatcher (mirrors `done`).
    let (xfer_done_tx, mut xfer_done_rx) = unbounded_channel::<(TransferId, TransferOutcome)>();

    // Owns the session credentials cached for auto-reconnect and the backoff loop.
    let mut reconnector = Reconnector::new(register_tx.clone(), done_tx.clone());

    // The throttle ticker for progress sampling. The first tick fires
    // immediately; on an idle loop it samples an empty set (cheap no-op).
    let mut progress_tick = tokio::time::interval(PROGRESS_TICK);

    loop {
        tokio::select! {
            maybe_cmd = commands.recv() => {
                let Some(command) = maybe_cmd else { break };
                match command {
                    Command::Shutdown => break,
                    Command::Connect { profile, secret, auto_reconnect } => {
                        if in_flight {
                            let _ = events.unbounded_send(Event::Error {
                                message: "a connection is already in progress".into(),
                            });
                            continue;
                        }
                        // A manual connect supersedes any backoff loop and re-seeds
                        // the credentials cached for a later auto-reconnect.
                        reconnector.abort();
                        reconnector.set_creds(profile.clone(), secret.clone(), auto_reconnect);
                        // Replace any existing session.
                        client = None;
                        in_flight = true;
                        tokio::spawn(run_task(
                            TaskKind::Connect,
                            profile,
                            secret,
                            events.clone(),
                            register_tx.clone(),
                            done_tx.clone(),
                        ));
                    }
                    Command::TestConnection { profile, secret } => {
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
                            secret,
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
                    // The result is computed first so the immutable session borrow
                    // ends before a `ConnectionLost` error needs `&mut client`.
                    Command::ListDir { path } => {
                        let result = match client.as_deref() {
                            Some(session) => Some(session.list_dir(&path).await),
                            None => None,
                        };
                        match result {
                            Some(Ok(entries)) => {
                                debug!(%path, count = entries.len(), "listed directory");
                                let _ = events.unbounded_send(Event::DirListing { path, entries });
                            }
                            Some(Err(err)) => report_op_error(
                                err, &mut client, &mut active_profile, &mut queue,
                                &mut last_bytes, &mut reconnector, &events,
                            ),
                            None => not_connected(&events),
                        }
                    }
                    Command::ResolveSymlink { path } => {
                        let result = match client.as_deref() {
                            Some(session) => Some(session.target_kind(&path).await),
                            None => None,
                        };
                        match result {
                            Some(Ok(kind)) => {
                                let _ = events.unbounded_send(Event::SymlinkResolved {
                                    path,
                                    is_dir: kind == EntryKind::Directory,
                                });
                            }
                            Some(Err(err)) => report_op_error(
                                err, &mut client, &mut active_profile, &mut queue,
                                &mut last_bytes, &mut reconnector, &events,
                            ),
                            None => not_connected(&events),
                        }
                    }
                    // Quick metadata ops: one SFTP round-trip, awaited inline.
                    Command::Mkdir { path } => {
                        let result = match client.as_deref() {
                            Some(session) => Some(session.mkdir(&path).await),
                            None => None,
                        };
                        match result {
                            Some(Ok(())) => {
                                let _ = events.unbounded_send(Event::FileOpDone {
                                    op: FileOp::Mkdir,
                                    message: format!("Created “{}”", base_name(&path)),
                                });
                            }
                            Some(Err(err)) => report_op_error(
                                err, &mut client, &mut active_profile, &mut queue,
                                &mut last_bytes, &mut reconnector, &events,
                            ),
                            None => not_connected(&events),
                        }
                    }
                    Command::Rename { from, to } => {
                        // Reject a rename that would race a live transfer on either
                        // endpoint (the path-lock policy).
                        if queue.is_remote_locked(&from) || queue.is_remote_locked(&to) {
                            let _ = events.unbounded_send(Event::Error {
                                message: format!("“{}” has a transfer in progress", base_name(&from)),
                            });
                        } else {
                            let result = match client.as_deref() {
                                Some(session) => Some(session.rename(&from, &to).await),
                                None => None,
                            };
                            match result {
                                Some(Ok(())) => {
                                    let _ = events.unbounded_send(Event::FileOpDone {
                                        op: FileOp::Rename,
                                        message: format!("Renamed to “{}”", base_name(&to)),
                                    });
                                }
                                Some(Err(err)) => report_op_error(
                                    err, &mut client, &mut active_profile, &mut queue,
                                    &mut last_bytes, &mut reconnector, &events,
                                ),
                                None => not_connected(&events),
                            }
                        }
                    }
                    // Slow ops: spawned against a cloned `Arc` so the loop stays
                    // responsive (and several can run at once); each emits its own
                    // terminal event. A missing session is reported immediately.
                    Command::Remove { path, is_dir } => {
                        // Reject a delete that would race a live transfer on the
                        // same path (the path-lock policy).
                        if queue.is_remote_locked(&path) {
                            let _ = events.unbounded_send(Event::Error {
                                message: format!("“{}” has a transfer in progress", base_name(&path)),
                            });
                        } else {
                            match client.clone() {
                                Some(session) => {
                                    let message = format!("Deleted “{}”", base_name(&path));
                                    spawn_file_op(FileOp::Remove, message, events.clone(), async move {
                                        let _ = is_dir; // protocol re-stats
                                        session.remove(&path).await
                                    });
                                }
                                None => not_connected(&events),
                            }
                        }
                    }
                    // Transfers go through the queue: submit → announce → try to
                    // start (subject to the cap). The dock row is the feedback —
                    // no `FileOpDone` toast for transfers.
                    Command::Download { remote, local, is_dir } => {
                        submit_transfer(
                            &mut queue, &client, &events, &xfer_done_tx,
                            TransferDirection::Download, kind_of(is_dir), remote, local,
                        );
                    }
                    Command::Upload { local, remote, is_dir } => {
                        submit_transfer(
                            &mut queue, &client, &events, &xfer_done_tx,
                            TransferDirection::Upload, kind_of(is_dir), remote, local,
                        );
                    }
                    Command::CancelTransfer { id } => match queue.cancel(id) {
                        // Never started: no task will report, so emit the terminal
                        // Cancelled directly.
                        CancelOutcome::WasQueued => {
                            last_bytes.remove(&id);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id,
                                status: TransferStatus::Cancelled,
                                message: None,
                            });
                        }
                        // Running: the copy loop notices the flag and reports
                        // through `xfer_done` on the normal terminal path.
                        CancelOutcome::WasRunning => {}
                        CancelOutcome::Unknown => {}
                    },
                    Command::ResolveCollision { id, choice, apply_to_all } => {
                        // Skip/Cancel resolutions terminate the parked items here
                        // (no task ran for them); Overwrite re-queues them, so a
                        // try_start picks them up.
                        let resolution = queue.resolve(id, choice, apply_to_all);
                        for sid in resolution.skipped {
                            last_bytes.remove(&sid);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id: sid,
                                status: TransferStatus::Skipped,
                                message: None,
                            });
                        }
                        for cid in resolution.cancelled {
                            last_bytes.remove(&cid);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id: cid,
                                status: TransferStatus::Cancelled,
                                message: None,
                            });
                        }
                        try_start(&mut queue, &client, &events, &xfer_done_tx);
                    }
                    Command::CancelReconnect => {
                        // Stop the backoff loop but keep the session credentials —
                        // a later manual reconnect re-seeds them anyway.
                        reconnector.abort();
                    }
                    Command::Disconnect => {
                        // A disconnect also clears the single-flight slot, the
                        // active-profile tracking and any auto-reconnect state.
                        in_flight = false;
                        active_profile = None;
                        reconnector.clear();
                        // Cancel everything: flag the running transfers (their
                        // tasks wind down and report Cancelled via `xfer_done`)
                        // and drain the queued ones (no task ran, so emit their
                        // terminal Cancelled here) — then drop the session.
                        for id in queue.cancel_all() {
                            last_bytes.remove(&id);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id,
                                status: TransferStatus::Cancelled,
                                message: None,
                            });
                        }
                        // Drop the shared session: its `Drop` closes the channel
                        // + connection. Any still-winding-down transfer task holds
                        // a clone that keeps the connection alive until its next
                        // cancel check ends the copy.
                        if client.take().is_some() {
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
                    TaskOutcome::Connected { profile_id, client: session, home } => {
                        // A connect (manual or via the backoff loop) landed; drop
                        // the loop's now-finished handle.
                        reconnector.abort();
                        client = Some(Arc::from(session));
                        active_profile = Some(profile_id.clone());
                        info!(%profile_id, "connected");
                        let _ = events.unbounded_send(Event::Connected { profile_id, home });
                    }
                    TaskOutcome::ConnectFailed { message } => {
                        // A failed manual connect holds no session to reconnect to.
                        reconnector.clear();
                        let _ = events.unbounded_send(Event::Error { message });
                    }
                    TaskOutcome::ReconnectFailed { profile_id, reason } => {
                        reconnector.clear();
                        warn!(%profile_id, "auto-reconnect gave up");
                        let _ = events.unbounded_send(Event::ReconnectFailed { profile_id, reason });
                    }
                    TaskOutcome::TestResult { profile_id, ok, message } => {
                        let _ = events.unbounded_send(Event::TestResult { profile_id, ok, message });
                    }
                }
            }
            Some((id, outcome)) = xfer_done_rx.recv() => {
                match outcome {
                    // The pre-flight gate found an existing destination: park the
                    // item and ask the UI. If there is no UI to answer (the event
                    // channel is closed), default to Skip — never silent overwrite.
                    TransferOutcome::Collision { existing_size } => {
                        if let Some(spec) = queue.park(id) {
                            let sent = events
                                .unbounded_send(Event::TransferCollision {
                                    id,
                                    direction: spec.direction,
                                    is_dir: spec.kind == TransferKind::Dir,
                                    remote: spec.remote.clone(),
                                    local: spec.local.display().to_string(),
                                    existing_size,
                                })
                                .is_ok();
                            if !sent {
                                for sid in queue.resolve(id, CollisionChoice::Skip, false).skipped {
                                    last_bytes.remove(&sid);
                                }
                            }
                        }
                    }
                    // A copy task finished: free its slot, drop its speed counter,
                    // announce the terminal state.
                    terminal => {
                        queue.finish(id);
                        last_bytes.remove(&id);
                        let (status, message) = match terminal {
                            TransferOutcome::Completed { message } => (TransferStatus::Completed, message),
                            TransferOutcome::Cancelled => (TransferStatus::Cancelled, None),
                            TransferOutcome::Skipped => (TransferStatus::Skipped, None),
                            TransferOutcome::Failed(msg) => (TransferStatus::Failed, Some(msg)),
                            TransferOutcome::Collision { .. } => unreachable!(),
                        };
                        let _ = events.unbounded_send(Event::TransferDone { id, status, message });
                    }
                }
                // Backfill any freed slot (a parked item frees its slot too).
                try_start(&mut queue, &client, &events, &xfer_done_tx);
            }
            _ = progress_tick.tick() => {
                // Sample every running transfer's byte counter and emit a
                // throttled progress event with an instantaneous speed.
                let samples: Vec<(TransferId, u64)> = queue.running_progress().collect();
                for (id, transferred) in samples {
                    let last = last_bytes.get(&id).copied().unwrap_or(0);
                    let delta = transferred.saturating_sub(last);
                    let speed_bps = delta * 1000 / PROGRESS_TICK.as_millis() as u64;
                    last_bytes.insert(id, transferred);
                    let _ = events.unbounded_send(Event::TransferProgress {
                        id,
                        transferred,
                        speed_bps,
                    });
                }
            }
        }
    }
}

/// The outcome of a spawned copy task, reported to the dispatcher.
enum TransferOutcome {
    /// The pre-flight gate found an existing destination and no resolved policy:
    /// the task wrote nothing and the dispatcher should park the item for a
    /// user decision. Not a terminal state.
    Collision {
        /// Size of the existing destination, if statted.
        existing_size: Option<u64>,
    },
    /// The copy finished and the remote writes were acknowledged. `message`
    /// carries a folder transfer's skipped/failed tally, if any.
    Completed { message: Option<String> },
    /// The copy was cancelled mid-flight (a partial file was cleaned up).
    Cancelled,
    /// The destination existed and the policy resolved to skip; nothing written.
    Skipped,
    /// The copy failed; the credential-free message is for the UI.
    Failed(String),
}

/// Map an `is_dir` flag to a [`TransferKind`].
fn kind_of(is_dir: bool) -> TransferKind {
    if is_dir {
        TransferKind::Dir
    } else {
        TransferKind::File
    }
}

/// Submit a transfer (file or directory) into the queue: guard on a live session,
/// build the spec, announce `TransferQueued`, then try to start it. Shared by the
/// `Download` and `Upload` commands (the only difference is direction).
#[allow(clippy::too_many_arguments)]
fn submit_transfer(
    queue: &mut TransferQueue,
    client: &Option<Arc<dyn RemoteClient>>,
    events: &FuturesSender<Event>,
    xfer_done: &TokioSender<(TransferId, TransferOutcome)>,
    direction: TransferDirection,
    kind: TransferKind,
    remote: RemotePath,
    local: PathBuf,
) {
    if client.is_none() {
        not_connected(events);
        return;
    }
    let spec = TransferSpec {
        direction,
        kind,
        remote: remote.clone(),
        local: local.clone(),
        on_collision: None,
    };
    match queue.submit(spec) {
        Ok(id) => {
            let _ = events.unbounded_send(Event::TransferQueued {
                id,
                direction,
                kind,
                remote,
                local: local.display().to_string(),
            });
            try_start(queue, client, events, xfer_done);
        }
        Err(_) => path_in_use(events, &remote),
    }
}

/// Promote and spawn as many queued transfers as the cap allows.
///
/// A missing session is a guard, not an error: queued transfers only exist while
/// connected (the senders check), and `Disconnect` drains the queue — so this is
/// just belt-and-braces against promoting a transfer with no session to run it.
fn try_start(
    queue: &mut TransferQueue,
    client: &Option<Arc<dyn RemoteClient>>,
    events: &FuturesSender<Event>,
    xfer_done: &TokioSender<(TransferId, TransferOutcome)>,
) {
    let Some(client) = client else { return };
    while let Some(started) = queue.poll_start() {
        spawn_transfer(client.clone(), started, events.clone(), xfer_done.clone());
    }
}

/// Spawn the copy task for a just-started transfer: stat the size, announce the
/// start, run the protocol copy, clean up any partial file on cancel/fail, and
/// report the terminal outcome back to the dispatcher.
fn spawn_transfer(
    client: Arc<dyn RemoteClient>,
    started: Started,
    events: FuturesSender<Event>,
    xfer_done: TokioSender<(TransferId, TransferOutcome)>,
) {
    let Started { id, spec, progress } = started;
    tokio::spawn(async move {
        // Pre-flight collision gate: stat the destination before writing a byte.
        // A reliability-first client must never blind-overwrite.
        if let Some(outcome) = collision_gate(&*client, &spec).await {
            let _ = xfer_done.send((id, outcome));
            return;
        }

        let outcome = match spec.kind {
            TransferKind::File => copy_file(&*client, &spec, &progress, id, &events).await,
            TransferKind::Dir => copy_dir(&*client, &spec, &progress, id, &events).await,
        };
        let _ = xfer_done.send((id, outcome));
    });
}

/// Copy a single file: stat the total, announce the start, run the protocol copy,
/// clean up any partial on cancel/fail.
async fn copy_file(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    progress: &nyx_core::TransferProgress,
    id: TransferId,
    events: &FuturesSender<Event>,
) -> TransferOutcome {
    // Stat the total up front so the dock can show a real %/total.
    let total = match spec.direction {
        TransferDirection::Download => client.remote_size(&spec.remote).await,
        TransferDirection::Upload => tokio::fs::metadata(&spec.local).await.ok().map(|m| m.len()),
    };
    let _ = events.unbounded_send(Event::TransferStarted { id, total });

    let result = match spec.direction {
        TransferDirection::Download => client.download(&spec.remote, &spec.local, progress).await,
        TransferDirection::Upload => client.upload(&spec.local, &spec.remote, progress).await,
    };
    match result {
        Ok(()) => TransferOutcome::Completed { message: None },
        Err(NyxError::Cancelled) => {
            cleanup_partial(client, spec).await;
            TransferOutcome::Cancelled
        }
        Err(err) => {
            cleanup_partial(client, spec).await;
            TransferOutcome::Failed(err.to_string())
        }
    }
}

/// Copy a whole directory tree as one aggregate transfer: enumerate it (so the
/// dock shows a real total), create the destination root, then walk the items
/// parent-before-child, reusing the single-file `download`/`upload` primitives.
///
/// Per the settled decisions: collisions merge (each file overwrites in place),
/// a failed/unreadable file is **skipped and tallied** (one bad file never aborts
/// the folder), symlinks are skipped during the walk, and empty directories are
/// created. Cancellation is checked between items; a cancelled or failed folder
/// leaves its partial tree in place (we never delete a merge destination).
async fn copy_dir(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    progress: &nyx_core::TransferProgress,
    id: TransferId,
    events: &FuturesSender<Event>,
) -> TransferOutcome {
    // Enumerate before announcing, so `total` is the real byte sum.
    let walk = match enumerate_dir(client, spec).await {
        Ok(walk) => walk,
        Err(err) => return TransferOutcome::Failed(err.to_string()),
    };
    let _ = events.unbounded_send(Event::TransferStarted {
        id,
        total: Some(walk.total_bytes),
    });

    // Create the destination root.
    if let Err(err) = make_root(client, spec).await {
        return TransferOutcome::Failed(err.to_string());
    }

    let mut failures = 0u64;
    for item in &walk.items {
        if progress.is_cancelled() {
            return TransferOutcome::Cancelled;
        }
        match copy_walk_item(client, spec, item, progress).await {
            Ok(()) => {}
            Err(NyxError::Cancelled) => return TransferOutcome::Cancelled,
            Err(err) => {
                debug!(error = %err, rel = ?item.rel, "skipping unreadable entry in folder transfer");
                failures += 1;
            }
        }
    }

    let mut notes = Vec::new();
    if failures > 0 {
        notes.push(format!("{failures} failed"));
    }
    if walk.skipped > 0 {
        notes.push(format!("{} skipped", walk.skipped));
    }
    let message = (!notes.is_empty()).then(|| notes.join(", "));
    TransferOutcome::Completed { message }
}

/// Enumerate a directory transfer's work items + totals — a remote walk for a
/// download, a local-filesystem walk for an upload.
async fn enumerate_dir(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
) -> Result<DirWalk, NyxError> {
    match spec.direction {
        TransferDirection::Download => client.walk_dir(&spec.remote).await,
        TransferDirection::Upload => local_walk(&spec.local).await,
    }
}

/// Create the destination root of a directory transfer (idempotent: an existing
/// root is fine — that is the merge case).
async fn make_root(client: &dyn RemoteClient, spec: &TransferSpec) -> Result<(), NyxError> {
    match spec.direction {
        TransferDirection::Download => tokio::fs::create_dir_all(&spec.local)
            .await
            .map_err(|e| NyxError::Io(e.to_string())),
        TransferDirection::Upload => ensure_remote_dir(client, &spec.remote).await,
    }
}

/// Copy one walk item to its mirrored destination under the transfer's root.
async fn copy_walk_item(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    item: &WalkItem,
    progress: &nyx_core::TransferProgress,
) -> Result<(), NyxError> {
    let remote = join_remote(&spec.remote, &item.rel);
    let local = join_local(&spec.local, &item.rel);
    match (spec.direction, item.is_dir) {
        (TransferDirection::Download, true) => tokio::fs::create_dir_all(&local)
            .await
            .map_err(|e| NyxError::Io(e.to_string())),
        (TransferDirection::Download, false) => client.download(&remote, &local, progress).await,
        (TransferDirection::Upload, true) => ensure_remote_dir(client, &remote).await,
        (TransferDirection::Upload, false) => client.upload(&local, &remote, progress).await,
    }
}

/// `mkdir` that tolerates an already-existing directory (the merge case).
async fn ensure_remote_dir(client: &dyn RemoteClient, path: &RemotePath) -> Result<(), NyxError> {
    match client.mkdir(path).await {
        Ok(()) => Ok(()),
        Err(err) => {
            if client.exists(path).await.unwrap_or(false) {
                Ok(())
            } else {
                Err(err)
            }
        }
    }
}

/// Join walk-item components onto a remote root.
fn join_remote(root: &RemotePath, rel: &[String]) -> RemotePath {
    rel.iter().fold(root.clone(), |p, seg| p.join(seg))
}

/// Join walk-item components onto a local root.
fn join_local(root: &std::path::Path, rel: &[String]) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in rel {
        p.push(seg);
    }
    p
}

/// Walk a local directory tree on the service thread, mirroring the remote
/// [`RemoteClient::walk_dir`]: pre-order, symlinks (and non-utf8 / special
/// entries) skipped and tallied, file sizes summed. No async recursion — an
/// explicit stack of directories to visit.
async fn local_walk(root: &std::path::Path) -> Result<DirWalk, NyxError> {
    let mut walk = DirWalk::default();
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(root.to_path_buf(), Vec::new())];
    while let Some((dir, rel)) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .map_err(|e| NyxError::Io(e.to_string()))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| NyxError::Io(e.to_string()))?
        {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                walk.skipped += 1; // non-UTF-8 names are not representable remotely
                continue;
            };
            // `symlink_metadata` is lstat-style, so a link is reported as a link.
            let meta = match tokio::fs::symlink_metadata(entry.path()).await {
                Ok(meta) => meta,
                Err(_) => {
                    walk.skipped += 1;
                    continue;
                }
            };
            let ft = meta.file_type();
            let mut child_rel = rel.clone();
            child_rel.push(name.to_string());
            if ft.is_symlink() || (!ft.is_dir() && !ft.is_file()) {
                walk.skipped += 1;
            } else if ft.is_dir() {
                walk.items.push(WalkItem {
                    rel: child_rel.clone(),
                    is_dir: true,
                    size: 0,
                });
                stack.push((entry.path(), child_rel));
            } else {
                walk.total_bytes += meta.len();
                walk.items.push(WalkItem {
                    rel: child_rel,
                    is_dir: false,
                    size: meta.len(),
                });
            }
        }
    }
    Ok(walk)
}

/// The transfer pre-flight gate. Stats the destination (remote for an upload,
/// local for a download) **before** any bytes move.
///
/// Returns `Some(outcome)` when the task must stop without writing:
/// - the destination exists and the policy is unresolved (`None`) → `Collision`
///   (the dispatcher parks the item and prompts);
/// - it exists and the policy is `Skip`/`Cancel` → the matching terminal.
///
/// Returns `None` to proceed with the copy — either no collision, or the policy
/// is `Overwrite`. A stat error is treated as "no collision" (proceed): it
/// mirrors the prior unconditional behavior rather than wedging the transfer.
async fn collision_gate(client: &dyn RemoteClient, spec: &TransferSpec) -> Option<TransferOutcome> {
    let exists = match spec.direction {
        TransferDirection::Download => tokio::fs::try_exists(&spec.local).await.unwrap_or(false),
        TransferDirection::Upload => client.exists(&spec.remote).await.unwrap_or(false),
    };
    if !exists {
        return None;
    }
    match spec.on_collision {
        None => {
            // A directory merge has no single "existing size"; only stat a file.
            let existing_size = match (spec.kind, spec.direction) {
                (TransferKind::Dir, _) => None,
                (TransferKind::File, TransferDirection::Download) => {
                    tokio::fs::metadata(&spec.local).await.ok().map(|m| m.len())
                }
                (TransferKind::File, TransferDirection::Upload) => {
                    client.remote_size(&spec.remote).await
                }
            };
            Some(TransferOutcome::Collision { existing_size })
        }
        Some(CollisionChoice::Skip) => Some(TransferOutcome::Skipped),
        Some(CollisionChoice::Cancel) => Some(TransferOutcome::Cancelled),
        Some(CollisionChoice::Overwrite) => None,
    }
}

/// Best-effort removal of the half-written file left by a cancelled/failed
/// transfer: the local destination for a download, the remote
/// destination for an upload. Errors are logged at `debug` and never surfaced —
/// the terminal `TransferDone` already tells the user the real story.
async fn cleanup_partial(client: &dyn RemoteClient, spec: &TransferSpec) {
    // A directory transfer may be merging into an existing tree, so deleting the
    // destination on cancel/fail could destroy pre-existing user data. Leave the
    // partial tree in place (the dock marks it Cancelled/Failed) and only ever
    // clean up the single half-written file of a file transfer.
    if spec.kind == TransferKind::Dir {
        return;
    }
    match spec.direction {
        TransferDirection::Download => {
            if let Err(err) = tokio::fs::remove_file(&spec.local).await {
                debug!(error = %err, "could not remove partial download");
            }
        }
        TransferDirection::Upload => {
            if let Err(err) = client.remove(&spec.remote).await {
                debug!(error = %err, "could not remove partial upload");
            }
        }
    }
}

/// Emit the standard "not connected" error (a file op arrived with no session).
fn not_connected(events: &FuturesSender<Event>) {
    let _ = events.unbounded_send(Event::Error {
        message: "not connected".into(),
    });
}

/// Emit the path-lock rejection for a transfer whose path already has a live one.
fn path_in_use(events: &FuturesSender<Event>, remote: &RemotePath) {
    let _ = events.unbounded_send(Event::Error {
        message: format!("“{}” already has a transfer in progress", base_name(remote)),
    });
}

/// Report an inline op's error: a transport death flips the session to "lost"
/// (via [`note_connection_lost`]); anything else is a plain [`Event::Error`].
fn report_op_error(
    err: NyxError,
    client: &mut Option<Arc<dyn RemoteClient>>,
    active_profile: &mut Option<String>,
    queue: &mut TransferQueue,
    last_bytes: &mut HashMap<TransferId, u64>,
    reconnector: &mut Reconnector,
    events: &FuturesSender<Event>,
) {
    if matches!(err, NyxError::ConnectionLost(_)) {
        note_connection_lost(
            client,
            active_profile,
            queue,
            last_bytes,
            reconnector,
            events,
            err.to_string(),
        );
    } else {
        let _ = events.unbounded_send(Event::Error {
            message: err.to_string(),
        });
    }
}

/// Flip the active session to "lost": drop the client (so later commands fail
/// fast), emit exactly one [`Event::ConnectionLost`], fail the **pending**
/// transfers (queued/parked can't run without a session; in-flight ones fail on
/// their own next I/O), and kick off an auto-reconnect backoff loop (a no-op when
/// the setting is off or no credentials are cached). The `client.take()` guard
/// makes this idempotent — a later op that also sees a transport error finds no
/// client and is a no-op.
fn note_connection_lost(
    client: &mut Option<Arc<dyn RemoteClient>>,
    active_profile: &mut Option<String>,
    queue: &mut TransferQueue,
    last_bytes: &mut HashMap<TransferId, u64>,
    reconnector: &mut Reconnector,
    events: &FuturesSender<Event>,
    reason: String,
) {
    if client.take().is_none() {
        return;
    }
    let profile_id = active_profile.take().unwrap_or_default();
    warn!(%profile_id, "connection lost");
    let _ = events.unbounded_send(Event::ConnectionLost { profile_id, reason });
    for id in queue.fail_pending() {
        last_bytes.remove(&id);
        let _ = events.unbounded_send(Event::TransferDone {
            id,
            status: TransferStatus::Failed,
            message: Some("connection lost".into()),
        });
    }
    reconnector.start(events);
}

/// The path's final component (the file/folder name) for toast copy, falling
/// back to `/` at the root. Paths are not secrets, so this is safe to surface.
fn base_name(path: &RemotePath) -> &str {
    path.file_name().unwrap_or("/")
}

/// Spawn a slow file op as a detached task: run `fut` against the cloned session,
/// then emit [`Event::FileOpDone`] on success or [`Event::Error`] on failure.
///
/// The op never touches the dispatcher's session slot or single-flight guard, so
/// several can run concurrently (`russh-sftp` multiplexes over the one channel).
fn spawn_file_op<F>(op: FileOp, message: String, events: FuturesSender<Event>, fut: F)
where
    F: Future<Output = nyx_core::Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        let event = match fut.await {
            Ok(()) => Event::FileOpDone { op, message },
            Err(err) => Event::Error {
                message: err.to_string(),
            },
        };
        let _ = events.unbounded_send(event);
    });
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
    secret: Secret,
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
    // Build the protocol client from the profile + carried secret. A profile-level
    // rejection (e.g. key auth on FTP) is reported here without ever connecting.
    let mut client = match build_client(&profile, secret, prompt) {
        Ok(client) => client,
        Err(err) => {
            let _ = done.send(connect_error_outcome(kind, profile_id, err));
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
    let _ = done.send(outcome);
}

/// Credentials cached for a live session's lifetime so an automatic reconnect
/// needs no UI round-trip per attempt. Held only while the session is alive and
/// dropped — zeroizing the [`Secret`] — on disconnect or when reconnect gives up.
struct SessionCreds {
    profile: Profile,
    secret: Secret,
    auto_reconnect: bool,
}

/// Owns the auto-reconnect state: the cached session credentials and the running
/// backoff task. The connection-loss path asks it to [`start`](Self::start) a
/// self-contained reconnect loop; the command loop can [`abort`](Self::abort) or
/// [`clear`](Self::clear) it.
struct Reconnector {
    creds: Option<SessionCreds>,
    task: Option<tokio::task::JoinHandle<()>>,
    register: TokioSender<oneshot::Sender<bool>>,
    done: TokioSender<TaskOutcome>,
}

impl Reconnector {
    fn new(register: TokioSender<oneshot::Sender<bool>>, done: TokioSender<TaskOutcome>) -> Self {
        Self {
            creds: None,
            task: None,
            register,
            done,
        }
    }

    /// Cache the credentials for the session being (re)connected.
    fn set_creds(&mut self, profile: Profile, secret: Secret, auto_reconnect: bool) {
        self.creds = Some(SessionCreds {
            profile,
            secret,
            auto_reconnect,
        });
    }

    /// Abort the running backoff loop, if any (keeps the cached credentials).
    fn abort(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    /// Abort the loop and drop the cached credentials (zeroizing the secret).
    fn clear(&mut self) {
        self.abort();
        self.creds = None;
    }

    /// Start a backoff reconnect loop for the lost session — but only when
    /// auto-reconnect is enabled and credentials are cached. A no-op otherwise,
    /// leaving the session lost for a manual reconnect.
    fn start(&mut self, events: &FuturesSender<Event>) {
        self.abort();
        let Some(creds) = self.creds.as_ref() else {
            return;
        };
        if !creds.auto_reconnect {
            return;
        }
        let task = tokio::spawn(run_reconnect(
            creds.profile.clone(),
            creds.secret.clone(),
            events.clone(),
            self.register.clone(),
            self.done.clone(),
        ));
        self.task = Some(task);
    }
}

/// Drive the auto-reconnect backoff loop for a lost session.
///
/// Each attempt emits [`Event::Reconnecting`], waits the backoff delay, then dials
/// the profile. A success hands the live session back via [`TaskOutcome::Connected`]
/// — the same path a manual connect uses. A *transport* failure is retried; an
/// auth / host-key / locked-key failure is terminal (retrying bad credentials is
/// pointless and can lock accounts). Exhausting the attempts ends in
/// [`TaskOutcome::ReconnectFailed`].
async fn run_reconnect(
    profile: Profile,
    secret: Secret,
    events: FuturesSender<Event>,
    register: TokioSender<oneshot::Sender<bool>>,
    done: TokioSender<TaskOutcome>,
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
                let _ = done.send(TaskOutcome::ReconnectFailed {
                    profile_id,
                    reason: err.to_string(),
                });
                return;
            }
        };
        match client.connect().await {
            Ok(()) => {
                let home = client
                    .default_dir()
                    .await
                    .unwrap_or_else(|_| RemotePath::root());
                let _ = done.send(TaskOutcome::Connected {
                    profile_id,
                    client,
                    home,
                });
                return;
            }
            Err(err) if is_transient_connect_error(&err) => {
                warn!(%profile_id, attempt, "auto-reconnect attempt failed; will retry");
            }
            Err(err) => {
                let _ = done.send(TaskOutcome::ReconnectFailed {
                    profile_id,
                    reason: err.to_string(),
                });
                return;
            }
        }
    }
    let _ = done.send(TaskOutcome::ReconnectFailed {
        profile_id,
        reason: "could not reconnect after several attempts".into(),
    });
}

/// Whether a failed connect attempt is worth retrying: a transport / network
/// failure (the server may still be down) is, but an auth, host-key or locked-key
/// rejection is not — see [`run_reconnect`].
fn is_transient_connect_error(err: &NyxError) -> bool {
    matches!(
        err,
        NyxError::Connection(_) | NyxError::ConnectionLost(_) | NyxError::Io(_)
    )
}

/// The backoff delay before attempt `n` (1-based): an exponential base (1s, 2s,
/// 4s … capped at [`RECONNECT_CAP`]) plus up to 50% jitter, so a flapping link or
/// many clients retrying don't hammer the server in lockstep.
fn backoff_delay(attempt: u32) -> Duration {
    let base = RECONNECT_CAP.min(Duration::from_secs(
        1u64 << attempt.saturating_sub(1).min(5),
    ));
    let base_ms = base.as_millis() as u64;
    Duration::from_millis(base_ms + jitter_ms(base_ms / 2))
}

/// A cheap, non-cryptographic jitter in `0..max_ms`, seeded from the wall clock —
/// used only to desynchronize backoff, so randomness quality is irrelevant.
fn jitter_ms(max_ms: u64) -> u64 {
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
fn build_client(
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
            Ok(Box::new(FtpClient::new(
                profile.host.clone(),
                profile.port,
                profile.username.clone(),
                secret.expose().to_string(),
            )))
        }
        Protocol::Ftps => {
            reject_key_auth(profile)?;
            Ok(Box::new(FtpsClient::new(
                profile.host.clone(),
                profile.port,
                profile.username.clone(),
                secret.expose().to_string(),
                profile.ftps_mode,
                KnownHosts::at(known_certs()),
                prompt,
            )))
        }
    }
}

/// Reject key auth for a non-SFTP protocol (FTP/FTPS are username+password only).
fn reject_key_auth(profile: &Profile) -> Result<(), NyxError> {
    if matches!(profile.auth, AuthMethod::Key { .. }) {
        return Err(NyxError::Other(
            "key authentication is only supported for SFTP".into(),
        ));
    }
    Ok(())
}

/// Map a pre-connect build error to the right terminal outcome for the task kind.
fn connect_error_outcome(kind: TaskKind, profile_id: String, err: NyxError) -> TaskOutcome {
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
/// (SFTP) and TLS certificates (FTPS) — the single decision slot is safe because
/// the single-flight guard allows only one connect-like op at a time.
mod host_key {
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

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_core::Protocol;

    #[test]
    fn connect_debug_redacts_the_secret() {
        let cmd = Command::Connect {
            profile: Profile {
                id: "id".into(),
                name: "n".into(),
                protocol: Protocol::Sftp,
                ftps_mode: Default::default(),
                host: "example.com".into(),
                port: 22,
                username: "user".into(),
                auth: AuthMethod::Password,
                remote_path: None,
                color: Default::default(),
                last_connected: None,
            },
            secret: Secret::new("hunter2"),
            auto_reconnect: true,
        };
        let dbg = format!("{cmd:?}");
        assert!(dbg.contains("***"), "{dbg}");
        assert!(!dbg.contains("hunter2"), "{dbg}");
    }

    #[test]
    fn backoff_grows_then_caps() {
        // Each attempt's delay (base, before jitter) doubles, then caps at 30s.
        // Jitter is at most +50% of the base, so an upper bound per attempt holds.
        for (attempt, base) in [(1u32, 1u64), (2, 2), (3, 4), (4, 8), (5, 16), (6, 30)] {
            let d = backoff_delay(attempt).as_millis() as u64;
            assert!(d >= base * 1000, "attempt {attempt}: {d} < {}", base * 1000);
            assert!(
                d <= base * 1000 + base * 500,
                "attempt {attempt}: {d} exceeds base+jitter"
            );
        }
        // Past the cap the base stays at 30s.
        assert!(backoff_delay(9).as_millis() as u64 <= 45_000);
    }

    #[test]
    fn only_transport_errors_are_retried() {
        assert!(is_transient_connect_error(&NyxError::Connection(
            "x".into()
        )));
        assert!(is_transient_connect_error(&NyxError::ConnectionLost(
            "x".into()
        )));
        assert!(is_transient_connect_error(&NyxError::Io("x".into())));
        // Credential / trust failures must not be retried.
        assert!(!is_transient_connect_error(&NyxError::Auth));
        assert!(!is_transient_connect_error(&NyxError::HostKey("x".into())));
        assert!(!is_transient_connect_error(&NyxError::KeyLocked));
    }
}
