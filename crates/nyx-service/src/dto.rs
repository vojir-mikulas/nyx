//! The service DTOs: the [`Command`] (UI -> service) and [`Event`] (service ->
//! UI) wire types, plus `FileOp` and `SearchHit`. The public API surface.
//!
//! Pure mechanical move out of `lib.rs` (code review 2026-06-08, plan 05).

use super::*;

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
