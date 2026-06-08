//! The Nyx backend service.
//!
//! GPUI runs its own executor on the main thread; `russh` (the SFTP transport) is
//! Tokio-based. So the backend lives on a **dedicated thread** that owns a Tokio
//! runtime, the active connection and (later) the transfer queue. The UI talks to
//! it over two channels:
//!
//! - [`Command`] - UI → service (sent synchronously from the GPUI thread over a
//!   Tokio mpsc; a send never blocks the UI).
//! - [`Event`] - service → UI. This side is a `futures::channel::mpsc` so the
//!   GPUI **foreground** executor can `await` it as a `Stream` inside `cx.spawn`
//!   (a blocking `std` recv there would freeze the UI).
//!
//! A single connection is supported (the active session); multi-session is out
//! of scope. [`Command::TestConnection`] / [`Event::TestResult`] back the
//! connection editor's "Test" button: the probe spins up a *transient* client
//! that never touches the stored session. A single-flight guard makes this safe:
//! at most one connect-like op (Connect or TestConnection) is in flight at a
//! time, so there is never more than one pending host-key decision.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;

use futures::channel::mpsc::{
    unbounded as futures_unbounded, UnboundedReceiver as FuturesReceiver,
    UnboundedSender as FuturesSender,
};
use futures::stream::{FuturesUnordered, StreamExt};
use nyx_core::{
    is_safe_local_segment, CollisionChoice, EntryIssue, EntryKind, Filter, NyxError, Permissions,
    Protocol, RemoteEntry, RemotePath, Secret, ServerTrustKind, TransferDirection, TransferId,
    TransferKind, TransferReport, TransferStatus, LARGE_LISTING_WARN,
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

/// The transfer concurrency cap for a protocol. FTP/FTPS keep a single stateful
/// control connection and serialize every operation over one stream mutex, so
/// more than one in-flight transfer can't actually run - cap them at 1 to keep
/// the queue (and the dock) honest rather than showing stalled "running" slots.
/// SFTP multiplexes channels and gets the full cap.
fn transfer_cap_for(protocol: Protocol) -> usize {
    match protocol {
        Protocol::Sftp => MAX_CONCURRENT_TRANSFERS,
        Protocol::Ftp | Protocol::Ftps => 1,
    }
}

/// How often the dispatcher samples running transfers' byte counters to emit a
/// throttled [`Event::TransferProgress`]. The fixed interval also serves as the
/// speed denominator, so no `Instant` is needed.
const PROGRESS_TICK: Duration = Duration::from_millis(150);

/// Recursion depth cap for a tree search (the root is depth 0). Bounds work on
/// pathological trees and is the symlink-loop backstop.
const SEARCH_MAX_DEPTH: u32 = 32;

/// Hard cap on hits a single tree search returns. Beyond this the result is
/// marked truncated and the walk stops - keeps a wildcard query over a huge tree
/// from flooding the UI. Lift once `large-listings` lazifies the row precompute.
const SEARCH_MAX_RESULTS: usize = 5_000;

/// Hits buffered before a streaming [`Event::SearchResult`] batch is flushed, so
/// results appear as the tree is walked rather than all at the end.
const SEARCH_BATCH: usize = 128;

/// How many directory listings a tree search keeps in flight at once. A deep
/// search is round-trip bound, and SFTP pipelines requests over its single
/// connection, so concurrency cuts wall-clock time sharply. FTP serializes
/// internally, so it simply ignores the extra parallelism - no benefit, no harm.
const SEARCH_CONCURRENCY: usize = 16;

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
/// (`<data_dir>/known_certs`) - the certificate parallel to [`known_hosts`].
fn known_certs() -> PathBuf {
    match directories::ProjectDirs::from("dev", "nyx", "Nyx") {
        Some(dirs) => dirs.data_dir().join("known_certs"),
        None => {
            warn!("could not resolve the OS data directory; using ./known_certs");
            PathBuf::from("known_certs")
        }
    }
}

/// A single match from a recursive tree search: an entry plus its absolute path
/// (results live outside the current directory, so the path is essential).
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// Absolute remote path of the matched entry.
    pub path: RemotePath,
    /// The matched entry's metadata.
    pub entry: RemoteEntry,
}

/// A request from the UI to the backend.
#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    /// Connect to `profile`, authenticating with `secret`.
    ///
    /// `secret` is the password or - for key auth - the key passphrase (empty for
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
    /// Recursively search the subtree at `root` on the active connection for
    /// entries matching `query`, streaming hits back as [`Event::SearchResult`].
    /// Runs off the command loop so the UI stays responsive; a later `SearchTree`
    /// or [`Command::CancelSearch`] supersedes an in-flight walk.
    SearchTree {
        /// Absolute remote path to search beneath (the search root).
        root: RemotePath,
        /// The parsed query each entry is matched against.
        query: Filter,
        /// Correlates streamed results to this request; the UI drops batches from
        /// a superseded (stale) token.
        token: u64,
    },
    /// Abort the in-flight tree search, if any. A no-op when none is running.
    CancelSearch,
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
    /// A batch of matches from a [`Command::SearchTree`] walk. Several arrive per
    /// search as the tree is streamed; the UI appends them under the matching
    /// `token` and drops batches from a stale (superseded) one.
    SearchResult {
        /// Echoes the request's token so the UI can drop stale results.
        token: u64,
        /// The matches in this batch (may be empty on the terminal batch).
        hits: Vec<SearchHit>,
        /// `true` on the final batch - the walk has finished (or was capped).
        done: bool,
        /// `true` when the result cap stopped the walk before the tree was
        /// exhausted, so the UI can say results are partial.
        truncated: bool,
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
    /// `Failed`, or a folder transfer's one-line skipped/failed summary for a
    /// `Completed`-with-issues.
    TransferDone {
        /// The transfer id.
        id: TransferId,
        /// The terminal status.
        status: TransferStatus,
        /// An error detail for `Failed`, or the one-line summary for a folder
        /// that completed with issues; `None` otherwise.
        message: Option<String>,
        /// The per-entry detail behind a folder transfer's summary (the paths
        /// that failed or were skipped, and why). `None` for file transfers and
        /// clean folders, so non-folder call sites ignore it.
        report: Option<TransferReport>,
    },
    /// A transfer was paused by a connection loss and is retained for resume on
    /// reconnect (it is *not* terminal). `transferred` is the bytes-done
    /// watermark so the dock keeps its progress instead of zeroing the bar.
    TransferInterrupted {
        /// The transfer id.
        id: TransferId,
        /// Cumulative bytes written so far (the resume watermark).
        transferred: u64,
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
    // Unbounded is deliberate and safe here: the only high-frequency producer is
    // the progress sampler, already coalesced to one `TransferProgress` per running
    // transfer per `PROGRESS_TICK` (≤ the concurrency cap, so a small constant
    // rate). Every other event is one-per-user-action or one-per-transfer-terminal
    // - naturally bounded by what the user did. So the queue can only grow if the
    // GPUI foreground executor stops draining entirely, which means the UI is
    // already wedged (a fatal condition, not backpressure to manage). Bounding here
    // would buy nothing but a risk of dropping a consequential terminal event.
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
    /// A live `Connect` - its session is kept on success.
    Connect,
    /// A `TestConnection` probe - the client is dropped after reporting.
    Test,
}

/// The result of a connect-like task, handed back to the dispatcher.
enum TaskOutcome {
    /// A live connect succeeded - the dispatcher takes ownership of the session.
    Connected {
        profile_id: String,
        /// The connected protocol, so the dispatcher can size the transfer
        /// concurrency cap (FTP/FTPS serialize over one connection → cap 1).
        protocol: Protocol,
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
    // Internal channel: a finished copy task → dispatcher (mirrors `done`). The
    // `u64` is the session generation the copy ran under (see `generation` below).
    let (xfer_done_tx, mut xfer_done_rx) =
        unbounded_channel::<(TransferId, u64, TransferOutcome)>();

    // Owns the session credentials cached for auto-reconnect and the backoff loop.
    let mut reconnector = Reconnector::new(register_tx.clone(), done_tx.clone());

    // Bumped on every successful connect. Each spawned copy task carries the
    // generation it ran under, so a straggler that only notices a drop *after* a
    // reconnect already succeeded can't be mistaken for a fresh loss of the new,
    // healthy session - it's resumed instead of flipping the session again.
    let mut generation: u64 = 0;

    // The in-flight tree search, if any: its cancel flag (stops the client walk
    // between listings) and its task handle (aborting drops a server-side `find`'s
    // channel, killing it). A new `SearchTree`/`CancelSearch`/connect/disconnect
    // ends it.
    let mut current_search: Option<(Arc<AtomicBool>, tokio::task::JoinHandle<()>)> = None;

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
                        // A manual connect supersedes any backoff loop. A manual
                        // *reconnect* (same profile) keeps interrupted transfers so
                        // they resume on `Connected`; connecting to a *different*
                        // profile cancels them - they belong to the old session.
                        reconnector.abort();
                        let same_profile = reconnector
                            .creds
                            .as_ref()
                            .map(|c| c.profile.id.as_str())
                            == Some(profile.id.as_str());
                        if !same_profile {
                            for id in queue.drain_interrupted() {
                                last_bytes.remove(&id);
                                let _ = events.unbounded_send(Event::TransferDone {
                                    id,
                                    status: TransferStatus::Cancelled,
                                    message: None,
                                    report: None,
                                });
                            }
                        }
                        reconnector.set_creds(profile.clone(), secret.clone(), auto_reconnect);
                        // Replace any existing session.
                        abort_search(&mut current_search);
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
                                let count = entries.len();
                                debug!(%path, count, "listed directory");
                                if count >= LARGE_LISTING_WARN {
                                    warn!(%path, count, "very large directory listing");
                                }
                                let _ = events.unbounded_send(Event::DirListing { path, entries });
                            }
                            Some(Err(err)) => report_op_error(
                                err, &mut client, &mut active_profile, &mut queue,
                                &mut last_bytes, &mut reconnector, &events,
                            ),
                            None => not_connected(&events),
                        }
                    }
                    Command::SearchTree { root, query, token } => {
                        // Supersede any in-flight search, then search off the command
                        // loop so further commands (incl. the next keystroke's
                        // search) keep flowing while the tree is traversed.
                        abort_search(&mut current_search);
                        match client.clone() {
                            Some(session) => {
                                let cancel = Arc::new(AtomicBool::new(false));
                                let events = events.clone();
                                let handle = tokio::spawn(run_tree_search(
                                    session,
                                    root,
                                    query,
                                    token,
                                    cancel.clone(),
                                    events,
                                ));
                                current_search = Some((cancel, handle));
                            }
                            None => not_connected(&events),
                        }
                    }
                    Command::CancelSearch => abort_search(&mut current_search),
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
                            tracing::info!(from = %from.as_str(), to = %to.as_str(), "rename: issuing");
                            let result = match client.as_deref() {
                                Some(session) => Some(session.rename(&from, &to).await),
                                None => None,
                            };
                            match result {
                                Some(Ok(())) => {
                                    tracing::info!(to = %to.as_str(), "rename: ok");
                                    let _ = events.unbounded_send(Event::FileOpDone {
                                        op: FileOp::Rename,
                                        message: format!("Renamed to “{}”", base_name(&to)),
                                    });
                                }
                                Some(Err(err)) => {
                                    tracing::warn!(error = %err, "rename: failed");
                                    report_op_error(
                                        err, &mut client, &mut active_profile, &mut queue,
                                        &mut last_bytes, &mut reconnector, &events,
                                    )
                                }
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
                    // start (subject to the cap). The dock row is the feedback -
                    // no `FileOpDone` toast for transfers.
                    Command::Download { remote, local, is_dir } => {
                        submit_transfer(
                            &mut queue, &client, &events, &xfer_done_tx, generation,
                            TransferDirection::Download, kind_of(is_dir), remote, local,
                        );
                    }
                    Command::Upload { local, remote, is_dir } => {
                        submit_transfer(
                            &mut queue, &client, &events, &xfer_done_tx, generation,
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
                                report: None,
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
                                report: None,
                            });
                        }
                        for cid in resolution.cancelled {
                            last_bytes.remove(&cid);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id: cid,
                                status: TransferStatus::Cancelled,
                                message: None,
                                report: None,
                            });
                        }
                        try_start(&mut queue, &client, &events, &xfer_done_tx, generation);
                    }
                    Command::CancelReconnect => {
                        // Stop the backoff loop but keep the session credentials -
                        // a later manual reconnect re-seeds them anyway.
                        reconnector.abort();
                    }
                    Command::Disconnect => {
                        // A disconnect also clears the single-flight slot, the
                        // active-profile tracking and any auto-reconnect state.
                        in_flight = false;
                        active_profile = None;
                        reconnector.clear();
                        abort_search(&mut current_search);
                        // Cancel everything: flag the running transfers (their
                        // tasks wind down and report Cancelled via `xfer_done`)
                        // and drain the queued ones (no task ran, so emit their
                        // terminal Cancelled here) - then drop the session.
                        for id in queue.cancel_all() {
                            last_bytes.remove(&id);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id,
                                status: TransferStatus::Cancelled,
                                message: None,
                                report: None,
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
                    TaskOutcome::Connected { profile_id, protocol, client: session, home } => {
                        // A connect (manual or via the backoff loop) landed; drop
                        // the loop's now-finished handle.
                        reconnector.abort();
                        client = Some(Arc::from(session));
                        active_profile = Some(profile_id.clone());
                        generation += 1;
                        // Size the concurrency cap to the protocol: FTP/FTPS
                        // serialize every op over one connection, so admitting more
                        // than one transfer only stalls them behind the stream lock.
                        queue.set_cap(transfer_cap_for(protocol));
                        info!(%profile_id, "connected");
                        let _ = events.unbounded_send(Event::Connected { profile_id, home });
                        // Resume any transfers interrupted by the loss, in their
                        // original order (a no-op on a first connect).
                        if queue.readmit_interrupted() > 0 {
                            try_start(&mut queue, &client, &events, &xfer_done_tx, generation);
                        }
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
            Some((id, gen, outcome)) = xfer_done_rx.recv() => {
                match outcome {
                    // The pre-flight gate found an existing destination: park the
                    // item and ask the UI. If there is no UI to answer (the event
                    // channel is closed), default to Skip - never silent overwrite.
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
                    // The transport died mid-copy: park this transfer in the
                    // resumable holding state, then decide what the loss means.
                    TransferOutcome::Interrupted { transferred, source_meta } => {
                        if queue.interrupt(id, transferred, source_meta) {
                            last_bytes.remove(&id);
                            let _ = events.unbounded_send(Event::TransferInterrupted { id, transferred });
                        }
                        if gen == generation {
                            // First sign of loss for the *current* session (a sibling
                            // may have flipped it already - note_connection_lost is
                            // idempotent). Flips the session, interrupts the pending
                            // transfers and starts the backoff loop.
                            note_connection_lost(
                                &mut client, &mut active_profile, &mut queue,
                                &mut last_bytes, &mut reconnector, &events,
                                "connection lost".into(),
                            );
                        } else if client.is_some() {
                            // A straggler from an already-replaced session noticing
                            // the *old* drop after we reconnected - don't flip the
                            // healthy session; re-admit it so the trailing try_start
                            // resumes it on the new one.
                            queue.readmit_interrupted();
                        }
                    }
                    // A copy task finished: free its slot, drop its speed counter,
                    // announce the terminal state.
                    terminal => {
                        queue.finish(id);
                        last_bytes.remove(&id);
                        let resolved = match terminal {
                            TransferOutcome::Completed { message, report } => {
                                Some((TransferStatus::Completed, message, report))
                            }
                            TransferOutcome::Cancelled => Some((TransferStatus::Cancelled, None, None)),
                            TransferOutcome::Skipped => Some((TransferStatus::Skipped, None, None)),
                            TransferOutcome::Failed(msg) => Some((TransferStatus::Failed, Some(msg), None)),
                            // Collision and Interrupted are handled by the arms above
                            // and never fall through here. Ignore rather than panic the
                            // backend thread if that invariant is ever broken.
                            TransferOutcome::Collision { .. } | TransferOutcome::Interrupted { .. } => {
                                warn!(?id, "unexpected non-terminal transfer outcome in terminal arm");
                                None
                            }
                        };
                        if let Some((status, message, report)) = resolved {
                            let _ = events.unbounded_send(Event::TransferDone { id, status, message, report });
                        }
                    }
                }
                // Backfill any freed slot (a parked item frees its slot too).
                try_start(&mut queue, &client, &events, &xfer_done_tx, generation);
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
    /// carries a folder transfer's one-line skipped/failed tally, if any;
    /// `report` carries the per-entry detail behind it (folder transfers only).
    Completed {
        message: Option<String>,
        report: Option<TransferReport>,
    },
    /// The copy was cancelled mid-flight (the temp partial was cleaned up).
    Cancelled,
    /// The destination existed and the policy resolved to skip; nothing written.
    Skipped,
    /// The transport died mid-copy: the partial is **kept** for a resume. Carries
    /// the bytes-done watermark and the source fingerprint captured at start.
    /// Only file transfers produce this; the dispatcher flips the session to
    /// lost and parks the transfer in the queue's interrupted state.
    Interrupted {
        /// Bytes written so far (the resume offset).
        transferred: u64,
        /// The source fingerprint at start, for the resume's unchanged-guard.
        source_meta: Option<nyx_core::SourceMeta>,
    },
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
    xfer_done: &TokioSender<(TransferId, u64, TransferOutcome)>,
    generation: u64,
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
        resume_from: 0,
        source_meta: None,
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
            try_start(queue, client, events, xfer_done, generation);
        }
        Err(_) => path_in_use(events, &remote),
    }
}

/// Promote and spawn as many queued transfers as the cap allows.
///
/// A missing session is a guard, not an error: queued transfers only exist while
/// connected (the senders check), and `Disconnect` drains the queue - so this is
/// just belt-and-braces against promoting a transfer with no session to run it.
fn try_start(
    queue: &mut TransferQueue,
    client: &Option<Arc<dyn RemoteClient>>,
    events: &FuturesSender<Event>,
    xfer_done: &TokioSender<(TransferId, u64, TransferOutcome)>,
    generation: u64,
) {
    let Some(client) = client else { return };
    while let Some(started) = queue.poll_start() {
        spawn_transfer(
            client.clone(),
            started,
            events.clone(),
            xfer_done.clone(),
            generation,
        );
    }
}

/// Spawn the copy task for a just-started transfer: stat the size, announce the
/// start, run the protocol copy into a sibling temp, rename it into place on
/// success (removing the temp on cancel/fail), and report the terminal outcome.
fn spawn_transfer(
    client: Arc<dyn RemoteClient>,
    started: Started,
    events: FuturesSender<Event>,
    xfer_done: TokioSender<(TransferId, u64, TransferOutcome)>,
    generation: u64,
) {
    let Started { id, spec, progress } = started;
    tokio::spawn(async move {
        // Pre-flight collision gate: stat the destination before writing a byte.
        // A reliability-first client must never blind-overwrite. A re-admitted
        // resume carries an `Overwrite` policy, so it skips the prompt here.
        if let Some(outcome) = collision_gate(&*client, &spec).await {
            let _ = xfer_done.send((id, generation, outcome));
            return;
        }

        let outcome = match spec.kind {
            TransferKind::File => copy_file(&*client, &spec, &progress, id, &events).await,
            TransferKind::Dir => copy_dir(&*client, &spec, &progress, id, &events).await,
        };
        let _ = xfer_done.send((id, generation, outcome));
    });
}

/// Copy a single file: capture the source fingerprint, decide the resume offset,
/// announce the start, run the protocol copy, and classify the outcome - a
/// transport death keeps the partial for a resume; any other error cleans it up.
async fn copy_file(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    progress: &nyx_core::TransferProgress,
    id: TransferId,
    events: &FuturesSender<Event>,
) -> TransferOutcome {
    // Fingerprint the source now. For a download the source is remote (only an
    // SFTP client reports it); for an upload it's the always-statable local file.
    // The fingerprint is carried into a resume to confirm the source is unchanged.
    let source_meta = capture_source_meta(client, spec).await;

    // Atomic destination: bytes are written to a sibling temp (`<name>.nyxpart`),
    // and the final path only ever appears via the atomic rename on success. A
    // cancelled/failed copy removes the temp; an interrupted one keeps it for the
    // resume - so the final path is never a half-written file masquerading as
    // complete, and a cancelled *overwrite* leaves the original intact.
    let tmp_local = local_part_path(&spec.local);
    let tmp_remote = remote_part_path(&spec.remote);

    // The effective offset is the **temp partial's actual on-disk size**, not the
    // watermark - the watermark can run ahead of durably-written bytes (an upload's
    // SFTP writes ack lazily), and resuming past the real EOF would leave a gap.
    // Only resume when the client can, the source is verifiably unchanged, and the
    // partial fits within it; otherwise restart from zero.
    let dest_size = if spec.resume_from > 0 {
        partial_temp_size(client, spec, &tmp_local, &tmp_remote).await
    } else {
        None
    };
    let offset = resume_offset(client.supports_resume(), spec, source_meta, dest_size);
    progress.seed(offset);

    // Stat the total up front so the dock can show a real %/total.
    let total = match spec.direction {
        TransferDirection::Download => client
            .remote_size(&spec.remote)
            .await
            .or_else(|| source_meta.map(|m| m.size)),
        TransferDirection::Upload => tokio::fs::metadata(&spec.local).await.ok().map(|m| m.len()),
    };
    let _ = events.unbounded_send(Event::TransferStarted { id, total });

    let result = match spec.direction {
        TransferDirection::Download => {
            client
                .download(&spec.remote, &tmp_local, progress, offset)
                .await
        }
        TransferDirection::Upload => {
            client
                .upload(&spec.local, &tmp_remote, progress, offset)
                .await
        }
    };
    match result {
        Ok(()) => {
            // Promote the temp into place. The rename is the overwrite for the
            // file case, so it fires only after the collision gate sanctioned it.
            let committed = match spec.direction {
                TransferDirection::Download => commit_local(&tmp_local, &spec.local).await,
                TransferDirection::Upload => {
                    commit_remote(client, &tmp_remote, &spec.remote, may_overwrite(spec)).await
                }
            };
            match committed {
                Ok(()) => TransferOutcome::Completed {
                    message: None,
                    report: None,
                },
                Err(err) => {
                    cleanup_file_temp(client, spec, &tmp_local, &tmp_remote).await;
                    TransferOutcome::Failed(err.to_string())
                }
            }
        }
        Err(NyxError::Cancelled) => {
            cleanup_file_temp(client, spec, &tmp_local, &tmp_remote).await;
            TransferOutcome::Cancelled
        }
        Err(err) => {
            // A transport loss is resumable: keep the temp partial, hand back the
            // watermark + fingerprint. A genuine error (disk full, permissions)
            // is terminal: clean up the temp. The probe disambiguates the two.
            if is_transport_lost(client, &spec.remote, &err).await {
                TransferOutcome::Interrupted {
                    transferred: progress.transferred(),
                    source_meta,
                }
            } else {
                cleanup_file_temp(client, spec, &tmp_local, &tmp_remote).await;
                TransferOutcome::Failed(err.to_string())
            }
        }
    }
}

/// Capture the source file's `(size, mtime)` fingerprint at the start of a copy,
/// used to guard a later resume. The source is the **remote** file for a download
/// (reported only by resume-capable clients) and the **local** file for an upload.
async fn capture_source_meta(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
) -> Option<nyx_core::SourceMeta> {
    match spec.direction {
        TransferDirection::Download => client.remote_meta(&spec.remote).await,
        TransferDirection::Upload => {
            let meta = tokio::fs::metadata(&spec.local).await.ok()?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            Some(nyx_core::SourceMeta {
                size: meta.len(),
                mtime,
            })
        }
    }
}

/// The current size of a copy's **temp partial** - the local temp for a
/// download, the remote temp for an upload. This is the source of truth for the
/// resume offset (the watermark can run ahead of durably-written bytes), and the
/// partial lives at the temp path, not the final one.
async fn partial_temp_size(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    tmp_local: &Path,
    tmp_remote: &RemotePath,
) -> Option<u64> {
    match spec.direction {
        TransferDirection::Download => tokio::fs::metadata(tmp_local).await.ok().map(|m| m.len()),
        TransferDirection::Upload => client.remote_size(tmp_remote).await,
    }
}

/// The byte offset a file copy should actually start from: the destination's
/// real `dest_size`, but only when this is a resume (`resume_from > 0`), the
/// client supports it, the source is verifiably unchanged (same size + mtime),
/// and the partial fits within the source. On any doubt - a changed source, a
/// missing mtime, an unverifiable fingerprint, an oversized partial - restart
/// from `0` rather than splice bytes blind. Silent corruption is worse than a
/// re-transfer.
fn resume_offset(
    supports_resume: bool,
    spec: &TransferSpec,
    current: Option<nyx_core::SourceMeta>,
    dest_size: Option<u64>,
) -> u64 {
    if spec.resume_from == 0 || !supports_resume {
        return 0;
    }
    match (spec.source_meta, current, dest_size) {
        (Some(orig), Some(cur), Some(dest))
            if orig == cur && orig.mtime.is_some() && dest <= cur.size =>
        {
            dest
        }
        _ => 0,
    }
}

/// Whether a failed file copy was a transport loss (→ resumable) rather than a
/// genuine error (→ terminal). An error already typed [`NyxError::ConnectionLost`]
/// is decisive; otherwise probe the session with a cheap stat - if that itself
/// reports the connection gone, the copy died with it.
async fn is_transport_lost(client: &dyn RemoteClient, remote: &RemotePath, err: &NyxError) -> bool {
    if matches!(err, NyxError::ConnectionLost(_)) {
        return true;
    }
    // The mid-copy byte loop surfaces a remote transport death as a generic I/O
    // error, so confirm with a probe: a live session answers (Ok), a dead one
    // maps to ConnectionLost.
    matches!(
        client.exists(remote).await,
        Err(NyxError::ConnectionLost(_))
    )
}

/// Copy a whole directory tree as one aggregate transfer: enumerate it (so the
/// dock shows a real total), create the destination root, then walk the items
/// parent-before-child, reusing the single-file `download`/`upload` primitives.
///
/// Per the settled decisions: collisions merge (each file overwrites in place),
/// a failed/unreadable file is **skipped and tallied** (one bad file never aborts
/// the folder), symlinks are skipped during the walk, and empty directories are
/// created. Each file rides the atomic temp-then-rename, so the tree never holds a
/// half-written file - only a subset of complete ones. Cancellation is checked
/// between items; a cancelled folder keeps the complete files it copied (we never
/// delete a merge destination), pruning only a root we created and left empty.
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

    // Create the destination root, remembering whether it pre-existed: a root we
    // created (no merge) is safe to prune back if the transfer is cancelled before
    // any file lands; a pre-existing merge target is the user's data - never touch.
    let created_root = match make_root(client, spec).await {
        Ok(created) => created,
        Err(err) => return TransferOutcome::Failed(err.to_string()),
    };

    let mut failed = 0u64;
    let mut issues: Vec<EntryIssue> = Vec::new();
    for item in &walk.items {
        if progress.is_cancelled() {
            prune_created_root(client, spec, created_root).await;
            return TransferOutcome::Cancelled;
        }
        match copy_walk_item(client, spec, item, progress).await {
            Ok(()) => {}
            Err(NyxError::Cancelled) => {
                prune_created_root(client, spec, created_root).await;
                return TransferOutcome::Cancelled;
            }
            Err(err) => {
                debug!(error = %err, rel = ?item.rel, "skipping unreadable entry in folder transfer");
                failed += 1;
                push_capped(
                    &mut issues,
                    EntryIssue::failed(item.rel.join("/"), err.to_string()),
                );
            }
        }
    }

    let skipped = walk.skips.len() as u64;
    for skip in walk.skips {
        push_capped(&mut issues, skip);
    }

    let report = TransferReport {
        failed,
        skipped,
        issues,
    };
    let message = report.summary();
    let report = report.has_issues().then_some(report);
    TransferOutcome::Completed { message, report }
}

/// Append `issue` to the retained list only while it is under the cap - full
/// counts stay exact, but a folder with thousands of bad entries never ships a
/// thousands-long report. The dropped tail is surfaced via
/// [`TransferReport::truncated`].
fn push_capped(issues: &mut Vec<EntryIssue>, issue: EntryIssue) {
    const ISSUE_CAP: usize = 100;
    if issues.len() < ISSUE_CAP {
        issues.push(issue);
    }
}

/// Enumerate a directory transfer's work items + totals - a remote walk for a
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
/// root is fine - that is the merge case). Returns `true` when the root did **not**
/// pre-exist (we created it), so a later cancel can safely prune it.
async fn make_root(client: &dyn RemoteClient, spec: &TransferSpec) -> Result<bool, NyxError> {
    match spec.direction {
        TransferDirection::Download => {
            let existed = tokio::fs::try_exists(&spec.local).await.unwrap_or(false);
            tokio::fs::create_dir_all(&spec.local)
                .await
                .map_err(|e| NyxError::Io(e.to_string()))?;
            Ok(!existed)
        }
        TransferDirection::Upload => {
            let existed = client.exists(&spec.remote).await.unwrap_or(false);
            ensure_remote_dir(client, &spec.remote).await?;
            Ok(!existed)
        }
    }
}

/// Copy one walk item to its mirrored destination under the transfer's root. File
/// items go through the atomic temp-then-rename so the tree never holds a
/// half-written file - only a subset of complete ones. A folder transfer is a
/// sanctioned merge, so an item may overwrite an existing file in the tree.
async fn copy_walk_item(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    item: &WalkItem,
    progress: &nyx_core::TransferProgress,
) -> Result<(), NyxError> {
    // Defense in depth: the walker already rejects unsafe names, but never let a
    // server-derived component reach a local `push` without re-checking - a `..`
    // or absolute segment must not escape the download destination.
    if spec.direction == TransferDirection::Download
        && !item.rel.iter().all(|seg| is_safe_local_segment(seg))
    {
        return Err(NyxError::Other(format!(
            "refusing unsafe destination path for {}",
            item.rel.join("/")
        )));
    }
    let remote = join_remote(&spec.remote, &item.rel);
    let local = join_local(&spec.local, &item.rel);
    match (spec.direction, item.is_dir) {
        (TransferDirection::Download, true) => tokio::fs::create_dir_all(&local)
            .await
            .map_err(|e| NyxError::Io(e.to_string())),
        // Directory transfers don't resume per-item yet - always copy from 0.
        (TransferDirection::Download, false) => {
            atomic_download_file(client, &remote, &local, progress).await
        }
        (TransferDirection::Upload, true) => ensure_remote_dir(client, &remote).await,
        (TransferDirection::Upload, false) => {
            atomic_upload_file(client, &local, &remote, progress).await
        }
    }
}

/// Download one file inside a folder transfer atomically: write to a sibling temp,
/// rename into place on success, and remove the temp on any error so the tree only
/// ever holds complete files.
async fn atomic_download_file(
    client: &dyn RemoteClient,
    remote: &RemotePath,
    local: &Path,
    progress: &nyx_core::TransferProgress,
) -> Result<(), NyxError> {
    let tmp = local_part_path(local);
    let result = match client.download(remote, &tmp, progress, 0).await {
        Ok(()) => commit_local(&tmp, local).await,
        Err(err) => Err(err),
    };
    if result.is_err() {
        remove_local_temp(&tmp).await;
    }
    result
}

/// Upload one file inside a folder transfer atomically (the upload mirror of
/// [`atomic_download_file`]); the merge permits overwriting an existing file.
async fn atomic_upload_file(
    client: &dyn RemoteClient,
    local: &Path,
    remote: &RemotePath,
    progress: &nyx_core::TransferProgress,
) -> Result<(), NyxError> {
    let tmp = remote_part_path(remote);
    let result = match client.upload(local, &tmp, progress, 0).await {
        Ok(()) => commit_remote(client, &tmp, remote, true).await,
        Err(err) => Err(err),
    };
    if result.is_err() {
        remove_remote_temp(client, &tmp).await;
    }
    result
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
/// entries) skipped and tallied, file sizes summed. No async recursion - an
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
            let raw_name = entry.file_name();
            let Some(name) = raw_name.to_str() else {
                // Non-UTF-8 names are not representable remotely - skip, but still
                // surface a (lossy) path so the report names the offending entry.
                let mut shown = rel.clone();
                shown.push(raw_name.to_string_lossy().into_owned());
                walk.skips
                    .push(EntryIssue::skipped(shown.join("/"), "non-UTF-8 name"));
                continue;
            };
            let mut child_rel = rel.clone();
            child_rel.push(name.to_string());
            // `symlink_metadata` is lstat-style, so a link is reported as a link.
            let meta = match tokio::fs::symlink_metadata(entry.path()).await {
                Ok(meta) => meta,
                Err(err) => {
                    walk.skips.push(EntryIssue::skipped(
                        child_rel.join("/"),
                        format!("unreadable: {err}"),
                    ));
                    continue;
                }
            };
            let ft = meta.file_type();
            if ft.is_symlink() {
                walk.skips
                    .push(EntryIssue::skipped(child_rel.join("/"), "symlink skipped"));
            } else if !ft.is_dir() && !ft.is_file() {
                walk.skips.push(EntryIssue::skipped(
                    child_rel.join("/"),
                    "special file skipped",
                ));
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
/// Returns `None` to proceed with the copy - either no collision, or the policy
/// is `Overwrite`.
///
/// A stat **error** (permission denied, transient I/O) is treated as *possibly
/// present*, not *absent* - so an unreadable destination prompts the user (or
/// honors a `Skip`) instead of being silently overwritten. Only a definite
/// `Ok(false)` skips the gate. A genuine `Overwrite` still proceeds regardless,
/// since the user already sanctioned replacing whatever is there.
async fn collision_gate(client: &dyn RemoteClient, spec: &TransferSpec) -> Option<TransferOutcome> {
    let exists = match spec.direction {
        TransferDirection::Download => treat_as_present(tokio::fs::try_exists(&spec.local).await),
        TransferDirection::Upload => treat_as_present(client.exists(&spec.remote).await),
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

/// How the collision gate reads a destination-existence probe. Only a definite
/// `Ok(false)` (the destination is absent) skips the gate; an error is treated as
/// *possibly present* so an unreadable destination is never silently overwritten -
/// it prompts (or honors `Skip`) instead. Pinned by a test so a future "simplify
/// to `unwrap_or(false)`" can't quietly reintroduce the blind-overwrite footgun.
fn treat_as_present<E>(probe: std::result::Result<bool, E>) -> bool {
    probe.unwrap_or(true)
}

/// The suffix marking a Nyx partial-transfer temp file. Deterministic and
/// recognizable so a crash-left temp can be identified (and a resume can find the
/// partial), never mistaken for user data and auto-deleted blindly.
const PART_SUFFIX: &str = ".nyxpart";

/// Sibling temp path for an atomic local write: `<name>.nyxpart` in the same
/// directory as `local`, so the rename into place is same-volume (atomic).
fn local_part_path(local: &Path) -> PathBuf {
    let mut name = local
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(PART_SUFFIX);
    local.with_file_name(name)
}

/// Sibling temp path for an atomic remote write - the remote mirror of
/// [`local_part_path`].
fn remote_part_path(remote: &RemotePath) -> RemotePath {
    match (remote.parent(), remote.file_name()) {
        (Some(parent), Some(name)) => parent.join(&format!("{name}{PART_SUFFIX}")),
        // The root has no name and is never a transfer target; leave it as-is.
        _ => remote.clone(),
    }
}

/// Whether a copy is allowed to replace an existing final destination: only when
/// the collision gate resolved to `Overwrite` (or a resume re-admitted with it).
fn may_overwrite(spec: &TransferSpec) -> bool {
    spec.on_collision == Some(CollisionChoice::Overwrite)
}

/// Promote a local temp into its final path. The rename atomically replaces any
/// existing file (same volume), so a cancelled overwrite leaves the original.
async fn commit_local(tmp: &Path, final_path: &Path) -> Result<(), NyxError> {
    tokio::fs::rename(tmp, final_path)
        .await
        .map_err(|e| NyxError::Io(e.to_string()))
}

/// Promote a remote temp into its final path. SFTP/FTP `rename` is the atomic
/// case, but some servers refuse to rename onto an existing path - so when (and
/// only when) overwriting was sanctioned and the final exists, remove it and
/// retry. Never a blind delete of a destination the user didn't choose to replace.
async fn commit_remote(
    client: &dyn RemoteClient,
    tmp: &RemotePath,
    final_path: &RemotePath,
    may_overwrite: bool,
) -> Result<(), NyxError> {
    match client.rename(tmp, final_path).await {
        Ok(()) => Ok(()),
        Err(err) => {
            if may_overwrite && client.exists(final_path).await.unwrap_or(false) {
                client.remove(final_path).await?;
                client.rename(tmp, final_path).await
            } else {
                Err(err)
            }
        }
    }
}

/// Best-effort removal of a cancelled/failed file transfer's temp partial: the
/// local temp for a download, the remote temp for an upload. The final path was
/// never touched (only a successful rename creates it), so there is nothing else
/// to clean. Errors are logged at `debug` - the terminal `TransferDone` tells the
/// real story.
async fn cleanup_file_temp(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    tmp_local: &Path,
    tmp_remote: &RemotePath,
) {
    match spec.direction {
        TransferDirection::Download => remove_local_temp(tmp_local).await,
        TransferDirection::Upload => remove_remote_temp(client, tmp_remote).await,
    }
}

/// Remove a local temp, ignoring a missing file (a cancel before any byte landed).
async fn remove_local_temp(tmp: &Path) {
    match tokio::fs::remove_file(tmp).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => debug!(error = %err, "could not remove partial download temp"),
    }
}

/// Remove a remote temp (best-effort; a missing temp is fine).
async fn remove_remote_temp(client: &dyn RemoteClient, tmp: &RemotePath) {
    if let Err(err) = client.remove(tmp).await {
        debug!(error = %err, "could not remove partial upload temp");
    }
}

/// On a cancelled folder transfer, remove the destination root **only if we
/// created it** (it didn't pre-exist, so it can't be a merge target holding the
/// user's data) **and it still holds no files** (every completed file is kept).
/// Conservative: a read error or any file present leaves the whole tree in place.
async fn prune_created_root(client: &dyn RemoteClient, spec: &TransferSpec, created_root: bool) {
    if !created_root {
        return;
    }
    let fileless = match spec.direction {
        TransferDirection::Download => local_tree_fileless(&spec.local).await,
        TransferDirection::Upload => remote_tree_fileless(client, &spec.remote).await,
    };
    if fileless != Some(true) {
        return;
    }
    match spec.direction {
        TransferDirection::Download => {
            if let Err(err) = tokio::fs::remove_dir_all(&spec.local).await {
                debug!(error = %err, "could not prune empty created download root");
            }
        }
        TransferDirection::Upload => {
            if let Err(err) = client.remove(&spec.remote).await {
                debug!(error = %err, "could not prune empty created upload root");
            }
        }
    }
}

/// Whether a local tree holds only (empty-of-files) directories - `None` on any
/// read error, so the caller leaves the tree alone when it can't be sure.
async fn local_tree_fileless(root: &Path) -> Option<bool> {
    let walk = local_walk(root).await.ok()?;
    Some(walk.items.iter().all(|i| i.is_dir) && walk.skips.is_empty())
}

/// The remote mirror of [`local_tree_fileless`].
async fn remote_tree_fileless(client: &dyn RemoteClient, root: &RemotePath) -> Option<bool> {
    let walk = client.walk_dir(root).await.ok()?;
    Some(walk.items.iter().all(|i| i.is_dir) && walk.skips.is_empty())
}

/// The one capability a tree search needs: list a directory. Going through a
/// narrow trait (rather than `RemoteClient` directly) keeps [`run_search`]
/// unit-testable against an in-memory fake.
#[async_trait]
trait DirLister: Send + Sync {
    async fn list(&self, path: &RemotePath) -> nyx_core::Result<Vec<RemoteEntry>>;
}

#[async_trait]
impl DirLister for Arc<dyn RemoteClient> {
    async fn list(&self, path: &RemotePath) -> nyx_core::Result<Vec<RemoteEntry>> {
        self.list_dir(path).await
    }
}

/// End the in-flight search (if any): flag the client walk to stop and abort the
/// task, which drops a server-side `find`'s channel and kills it.
fn abort_search(current: &mut Option<(Arc<AtomicBool>, tokio::task::JoinHandle<()>)>) {
    if let Some((flag, handle)) = current.take() {
        flag.store(true, Ordering::Relaxed);
        handle.abort();
    }
}

/// Run a tree search: try to offload it to the server (`find` over SSH `exec`),
/// and fall back to the client-side walk when the protocol/server can't - FTP, a
/// jailed sftp-only server, or a query with `size:`/`modified:` terms `find`
/// can't express here.
async fn run_tree_search(
    client: Arc<dyn RemoteClient>,
    root: RemotePath,
    query: Filter,
    token: u64,
    cancel: Arc<AtomicBool>,
    events: FuturesSender<Event>,
) {
    if let Some(predicates) = query.as_find_predicates() {
        // `Ok(None)` (unsupported) or `Err` (failed exec) → fall through to the
        // client walk; only a `Some(paths)` short-circuits.
        if let Ok(Some(paths)) = client
            .server_search(&root, &predicates, SEARCH_MAX_RESULTS)
            .await
        {
            emit_find_results(&events, token, &predicates, paths);
            return;
        }
    }
    run_search(&client, root, query, token, cancel, events).await;
}

/// Stream server-`find` matches to the UI. The paths carry no metadata, so each
/// entry is synthesized - kind from a `-type` predicate when present (else file),
/// size/mtime unknown (the UI renders those as "—").
fn emit_find_results(
    events: &FuturesSender<Event>,
    token: u64,
    predicates: &[nyx_core::FindPredicate],
    paths: Vec<RemotePath>,
) {
    use nyx_core::FindPredicate;
    let kind = predicates
        .iter()
        .find_map(|p| match p {
            FindPredicate::Kind(k) => Some(*k),
            _ => None,
        })
        .unwrap_or(EntryKind::File);
    let truncated = paths.len() >= SEARCH_MAX_RESULTS;

    let mut remaining: Vec<SearchHit> = paths
        .into_iter()
        .map(|path| {
            let name = path.file_name().unwrap_or_default().to_string();
            SearchHit {
                entry: RemoteEntry {
                    name,
                    size: 0,
                    kind,
                    modified: None,
                    permissions: Permissions::from_mode(0),
                },
                path,
            }
        })
        .collect();

    // Stream in the same batch size the walk uses, ending with a terminal `done`
    // batch (empty when there were no matches at all).
    loop {
        let take = remaining.len().min(SEARCH_BATCH);
        let chunk: Vec<SearchHit> = remaining.drain(..take).collect();
        let done = remaining.is_empty();
        let _ = events.unbounded_send(Event::SearchResult {
            token,
            hits: chunk,
            done,
            truncated: done && truncated,
        });
        if done {
            break;
        }
    }
}

/// Breadth-first walk of `root`, streaming matched entries back in batches.
///
/// Up to [`SEARCH_CONCURRENCY`] directory listings run **in flight at once** -
/// the dominant cost of a deep search is sequential round-trips, and SFTP
/// multiplexes requests over its one connection, so concurrency is the big win.
/// Bounded by [`SEARCH_MAX_DEPTH`] (also the symlink-loop backstop) and
/// [`SEARCH_MAX_RESULTS`]. A directory that fails to list (permission denied, a
/// vanished path) is skipped, not fatal. A completed directory's matches are
/// flushed right away so results appear as they're found, not only at the end.
/// The walk checks `cancel` each turn and bails when a newer search supersedes
/// it - the UI ignores that token anyway, so no terminal batch is owed.
async fn run_search(
    client: &(impl DirLister + ?Sized),
    root: RemotePath,
    query: Filter,
    token: u64,
    cancel: Arc<AtomicBool>,
    events: FuturesSender<Event>,
) {
    let now = SystemTime::now();
    let mut frontier: VecDeque<(RemotePath, u32)> = VecDeque::new();
    frontier.push_back((root, 0));
    let mut inflight = FuturesUnordered::new();
    let mut batch: Vec<SearchHit> = Vec::new();
    let mut found = 0usize;
    let mut truncated = false;

    'walk: loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        // Keep the connection busy: top in-flight listings up to the cap.
        while inflight.len() < SEARCH_CONCURRENCY {
            let Some((dir, depth)) = frontier.pop_front() else {
                break;
            };
            inflight.push(async move {
                let entries = client.list(&dir).await;
                (dir, depth, entries)
            });
        }
        // Frontier drained and nothing in flight → the walk is done.
        let Some((dir, depth, result)) = inflight.next().await else {
            break;
        };
        let Ok(entries) = result else {
            continue; // unreadable directory: skip, keep searching
        };
        for entry in entries {
            let name_lower = entry.name.to_lowercase();
            let path = dir.join(&entry.name);
            if entry.is_dir() && depth < SEARCH_MAX_DEPTH {
                frontier.push_back((path.clone(), depth + 1));
            }
            if query.matches(&entry, &name_lower, now) {
                batch.push(SearchHit { path, entry });
                found += 1;
                if batch.len() >= SEARCH_BATCH {
                    flush_hits(&events, token, &mut batch, false, false);
                }
                if found >= SEARCH_MAX_RESULTS {
                    truncated = true;
                    break 'walk;
                }
            }
        }
        // Stream this directory's matches now, rather than waiting for the cap.
        if !batch.is_empty() {
            flush_hits(&events, token, &mut batch, false, false);
        }
    }
    flush_hits(&events, token, &mut batch, true, truncated);
}

/// Send one [`Event::SearchResult`] batch, draining `batch`.
fn flush_hits(
    events: &FuturesSender<Event>,
    token: u64,
    batch: &mut Vec<SearchHit>,
    done: bool,
    truncated: bool,
) {
    let hits = std::mem::take(batch);
    let _ = events.unbounded_send(Event::SearchResult {
        token,
        hits,
        done,
        truncated,
    });
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
/// fast), emit exactly one [`Event::ConnectionLost`], move the **pending**
/// transfers (queued/parked) into the resumable interrupted state, and kick off
/// an auto-reconnect backoff loop (a no-op when the setting is off or no
/// credentials are cached). In-flight transfers interrupt themselves as their
/// copy tasks notice the drop. The `client.take()` guard makes this idempotent -
/// a later op that also sees a transport error finds no client and is a no-op.
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
    for id in queue.interrupt_pending() {
        last_bytes.remove(&id);
        let _ = events.unbounded_send(Event::TransferInterrupted { id, transferred: 0 });
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
    let _ = done.send(outcome);
}

/// Credentials cached for a live session's lifetime so an automatic reconnect
/// needs no UI round-trip per attempt. Held only while the session is alive and
/// dropped - zeroizing the [`Secret`] - on disconnect or when reconnect gives up.
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

    /// Start a backoff reconnect loop for the lost session - but only when
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
/// the profile. A success hands the live session back via [`TaskOutcome::Connected`],
/// the same path a manual connect uses. A *transport* failure is retried; an
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
                    protocol: profile.protocol,
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
/// rejection is not - see [`run_reconnect`].
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

/// A cheap, non-cryptographic jitter in `0..max_ms`, seeded from the wall clock -
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
const ANON_PASSWORD: &str = "anonymous@";

/// Resolve the `(username, password)` an FTP/FTPS login should send. Anonymous
/// ignores the stored username and any secret; otherwise the profile username and
/// the exposed secret are used.
fn ftp_credentials(profile: &Profile, secret: &Secret) -> (String, String) {
    match profile.auth {
        AuthMethod::Anonymous => ("anonymous".to_string(), ANON_PASSWORD.to_string()),
        _ => (profile.username.clone(), secret.expose().to_string()),
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
/// (SFTP) and TLS certificates (FTPS) - the single decision slot is safe because
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

    fn profile_with(protocol: Protocol, auth: AuthMethod) -> Profile {
        Profile {
            id: "id".into(),
            name: "n".into(),
            protocol,
            ftps_mode: Default::default(),
            host: "example.com".into(),
            port: 21,
            username: "user".into(),
            auth,
            remote_path: None,
            color: Default::default(),
            last_connected: None,
        }
    }

    #[test]
    fn anonymous_ftp_login_ignores_username_and_any_secret() {
        let profile = profile_with(Protocol::Ftp, AuthMethod::Anonymous);
        let (user, pass) = ftp_credentials(&profile, &Secret::new("ignored"));
        assert_eq!(user, "anonymous");
        assert_eq!(pass, ANON_PASSWORD);
    }

    #[test]
    fn password_ftp_login_uses_profile_username_and_secret() {
        let profile = profile_with(Protocol::Ftp, AuthMethod::Password);
        let (user, pass) = ftp_credentials(&profile, &Secret::new("hunter2"));
        assert_eq!(user, "user");
        assert_eq!(pass, "hunter2");
    }

    #[test]
    fn anonymous_is_rejected_for_sftp() {
        let (events_tx, _events_rx) = futures::channel::mpsc::unbounded();
        let (register_tx, _register_rx) = unbounded_channel();
        let prompt = Arc::new(host_key::PromptBridge {
            events: events_tx,
            register: register_tx,
        });
        let profile = profile_with(Protocol::Sftp, AuthMethod::Anonymous);
        assert!(build_client(&profile, Secret::new(""), prompt).is_err());
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

    #[test]
    fn part_paths_are_siblings_keeping_the_full_name() {
        // The temp sits in the same directory (same volume → atomic rename) and
        // appends - never replaces - the extension, so `foo.txt` stays distinct
        // from a sibling `foo.tar`.
        assert_eq!(
            local_part_path(Path::new("/home/u/foo.txt")),
            PathBuf::from("/home/u/foo.txt.nyxpart")
        );
        assert_eq!(
            remote_part_path(&RemotePath::new("/srv/data/foo.txt")),
            RemotePath::new("/srv/data/foo.txt.nyxpart")
        );
    }

    #[test]
    fn ftp_protocols_cap_transfers_at_one() {
        // FTP/FTPS serialize over one connection → cap 1; SFTP gets the full cap.
        assert_eq!(transfer_cap_for(Protocol::Sftp), MAX_CONCURRENT_TRANSFERS);
        assert_eq!(transfer_cap_for(Protocol::Ftp), 1);
        assert_eq!(transfer_cap_for(Protocol::Ftps), 1);
    }

    #[test]
    fn stat_error_counts_as_present_not_absent() {
        // A definite "absent" is the only result that skips the collision gate.
        assert!(!treat_as_present::<std::io::Error>(Ok(false)));
        assert!(treat_as_present::<std::io::Error>(Ok(true)));
        // An error must NOT degrade to "absent" → blind overwrite.
        assert!(treat_as_present(Err::<bool, _>(std::io::Error::other("x"))));
    }

    #[test]
    fn may_overwrite_only_on_resolved_overwrite() {
        assert!(may_overwrite(&resume_spec(0, None)));
        let mut spec = resume_spec(0, None);
        spec.on_collision = None;
        assert!(!may_overwrite(&spec));
        spec.on_collision = Some(CollisionChoice::Skip);
        assert!(!may_overwrite(&spec));
    }

    fn resume_spec(resume_from: u64, source_meta: Option<nyx_core::SourceMeta>) -> TransferSpec {
        TransferSpec {
            direction: TransferDirection::Download,
            kind: TransferKind::File,
            remote: RemotePath::new("/r/f"),
            local: std::path::PathBuf::from("/l/f"),
            on_collision: Some(CollisionChoice::Overwrite),
            resume_from,
            source_meta,
        }
    }

    #[test]
    fn resume_only_when_supported_and_source_unchanged() {
        let meta = nyx_core::SourceMeta {
            size: 1000,
            mtime: Some(42),
        };
        // Fresh transfer (offset 0) never resumes.
        assert_eq!(
            resume_offset(true, &resume_spec(0, Some(meta)), Some(meta), Some(256)),
            0
        );
        // A resume-capable client with an unchanged source resumes from the
        // destination's actual size (256), not the recorded watermark (300).
        assert_eq!(
            resume_offset(true, &resume_spec(300, Some(meta)), Some(meta), Some(256)),
            256
        );
        // A non-resume-capable client always restarts.
        assert_eq!(
            resume_offset(false, &resume_spec(300, Some(meta)), Some(meta), Some(256)),
            0
        );
    }

    #[test]
    fn resume_restarts_on_any_doubt() {
        let orig = nyx_core::SourceMeta {
            size: 1000,
            mtime: Some(42),
        };
        // Size changed under us → restart.
        let bigger = nyx_core::SourceMeta {
            size: 2000,
            mtime: Some(42),
        };
        assert_eq!(
            resume_offset(true, &resume_spec(256, Some(orig)), Some(bigger), Some(256)),
            0
        );
        // mtime changed → restart.
        let touched = nyx_core::SourceMeta {
            size: 1000,
            mtime: Some(99),
        };
        assert_eq!(
            resume_offset(
                true,
                &resume_spec(256, Some(orig)),
                Some(touched),
                Some(256)
            ),
            0
        );
        // Source unstattable now → restart.
        assert_eq!(
            resume_offset(true, &resume_spec(256, Some(orig)), None, Some(256)),
            0
        );
        // No fingerprint captured at start → restart.
        assert_eq!(
            resume_offset(true, &resume_spec(256, None), Some(orig), Some(256)),
            0
        );
        // Destination unstattable → restart.
        assert_eq!(
            resume_offset(true, &resume_spec(256, Some(orig)), Some(orig), None),
            0
        );
        // mtime unknown on both sides (equal but unverifiable) → restart.
        let no_mtime = nyx_core::SourceMeta {
            size: 1000,
            mtime: None,
        };
        assert_eq!(
            resume_offset(
                true,
                &resume_spec(256, Some(no_mtime)),
                Some(no_mtime),
                Some(256)
            ),
            0
        );
        // Partial larger than the current source (truncated/replaced) → restart.
        assert_eq!(
            resume_offset(true, &resume_spec(256, Some(orig)), Some(orig), Some(1200)),
            0
        );
    }

    use nyx_core::Permissions;

    /// In-memory directory tree for [`run_search`] tests. A path absent from the
    /// map lists as an error - modeling an unreadable (skipped) directory.
    struct FakeTree {
        dirs: HashMap<String, Vec<RemoteEntry>>,
    }

    #[async_trait]
    impl DirLister for FakeTree {
        async fn list(&self, path: &RemotePath) -> nyx_core::Result<Vec<RemoteEntry>> {
            self.dirs
                .get(path.as_str())
                .cloned()
                .ok_or_else(|| NyxError::NotFound(path.as_str().into()))
        }
    }

    fn dir_entry(name: &str) -> RemoteEntry {
        RemoteEntry {
            name: name.into(),
            size: 0,
            kind: EntryKind::Directory,
            modified: None,
            permissions: Permissions::from_mode(0o755),
        }
    }

    fn file_entry(name: &str) -> RemoteEntry {
        RemoteEntry {
            name: name.into(),
            size: 0,
            kind: EntryKind::File,
            modified: None,
            permissions: Permissions::from_mode(0o644),
        }
    }

    /// Run a search to completion and gather all streamed hits.
    async fn collect_search(
        tree: &FakeTree,
        query: &str,
        cancel: Arc<AtomicBool>,
    ) -> (Vec<SearchHit>, bool, bool) {
        let (tx, mut rx) = futures_unbounded();
        run_search(
            tree,
            RemotePath::root(),
            Filter::parse(query),
            9,
            cancel,
            tx,
        )
        .await;
        // `run_search` owns and drops the sender on return, so the receiver is now
        // closed and drains the buffered batches.
        let (mut hits, mut done, mut truncated) = (Vec::new(), false, false);
        while let Some(event) = rx.next().await {
            if let Event::SearchResult {
                token,
                hits: batch,
                done: d,
                truncated: t,
            } = event
            {
                assert_eq!(token, 9);
                hits.extend(batch);
                done |= d;
                truncated |= t;
            }
        }
        (hits, done, truncated)
    }

    fn sample_tree() -> FakeTree {
        let mut dirs = HashMap::new();
        dirs.insert(
            "/".to_string(),
            vec![
                dir_entry("src"),
                file_entry("readme.md"),
                dir_entry("denied"),
            ],
        );
        dirs.insert(
            "/src".to_string(),
            vec![
                file_entry("main.rs"),
                file_entry("lib.rs"),
                dir_entry("deep"),
            ],
        );
        dirs.insert("/src/deep".to_string(), vec![file_entry("mod.rs")]);
        // "/denied" is intentionally absent → lists as an error (skipped).
        FakeTree { dirs }
    }

    #[tokio::test]
    async fn search_walks_subtree_and_matches() {
        let (hits, done, truncated) =
            collect_search(&sample_tree(), "*.rs", Arc::new(AtomicBool::new(false))).await;
        let mut paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, ["/src/deep/mod.rs", "/src/lib.rs", "/src/main.rs"]);
        assert!(done);
        assert!(!truncated);
    }

    #[tokio::test]
    async fn search_skips_unreadable_dirs_and_completes() {
        // The walk descends into the absent "/denied" dir, which errors; it must
        // be skipped rather than aborting the whole search.
        let (hits, done, _) = collect_search(
            &sample_tree(),
            "type:file",
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        assert!(done);
        assert_eq!(hits.len(), 4); // readme.md, main.rs, lib.rs, mod.rs
    }

    #[tokio::test]
    async fn search_respects_result_cap() {
        let many: Vec<RemoteEntry> = (0..SEARCH_MAX_RESULTS + 5)
            .map(|i| file_entry(&format!("f{i}.txt")))
            .collect();
        let mut dirs = HashMap::new();
        dirs.insert("/".to_string(), many);
        let tree = FakeTree { dirs };

        let (hits, done, truncated) =
            collect_search(&tree, "*.txt", Arc::new(AtomicBool::new(false))).await;
        assert_eq!(hits.len(), SEARCH_MAX_RESULTS);
        assert!(truncated);
        assert!(done);
    }

    #[tokio::test]
    async fn cancelled_search_emits_nothing() {
        let cancel = Arc::new(AtomicBool::new(true));
        let (hits, done, _) = collect_search(&sample_tree(), "*.rs", cancel).await;
        assert!(hits.is_empty());
        assert!(!done, "a superseded search owes no terminal batch");
    }
}
