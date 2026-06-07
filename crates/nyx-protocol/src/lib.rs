//! Protocol abstraction for Nyx.
//!
//! [`RemoteClient`] is the single async trait every protocol implements:
//! [`SftpClient`] (SSH), [`FtpClient`] (plain FTP) and [`FtpsClient`] (FTP over
//! TLS). The service selects one at runtime from `profile.protocol`; the UI and
//! transfer layers never see the concrete type.
//!
//! The trait is made object-safe with `async_trait` so the service can hold a
//! `Box<dyn RemoteClient>` selected at runtime by [`nyx_core::Protocol`].

use std::path::Path;

use async_trait::async_trait;
use nyx_core::{EntryIssue, EntryKind, RemoteEntry, RemotePath, Result, TransferProgress};

mod ftp;
mod ftps;
mod host_key;
mod known_hosts;
mod sftp;
mod util;

pub use ftp::FtpClient;
pub use ftps::FtpsClient;
pub use host_key::ServerTrustPrompt;
pub use known_hosts::{KnownHostStatus, KnownHosts};
pub use sftp::{Auth, SftpClient};

/// One entry discovered by a recursive directory walk, addressed **relative to
/// the walk root** so the same item describes a source and its mirrored
/// destination. Items come back parent-before-child: a directory always precedes
/// anything beneath it, so applying them in order creates each parent before its
/// children (`mkdir` / `create_dir_all`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkItem {
    /// Path components relative to the walk root, e.g. `["sub", "c.txt"]`. Empty
    /// only for the root itself, which `walk_dir` never emits.
    pub rel: Vec<String>,
    /// Whether this item is a directory (create it) or a file (copy it).
    pub is_dir: bool,
    /// File size in bytes; `0` for a directory.
    pub size: u64,
}

/// The result of a recursive directory walk: the work items plus the totals the
/// caller needs to announce an accurate transfer up front.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirWalk {
    /// Items in parent-before-child order (directories before their contents).
    pub items: Vec<WalkItem>,
    /// Sum of all file sizes — the transfer's `total`.
    pub total_bytes: u64,
    /// Entries skipped during the walk — symlinks, special files and (locally)
    /// non-UTF-8 names — each carrying *why*, so the terminal report lists the
    /// path and reason rather than a bare count. We don't follow links in v1.
    pub skips: Vec<EntryIssue>,
}

/// An async client for a remote filesystem.
///
/// Implementations own a single connection. Methods borrow `&self` for reads and
/// transfers (so they can run concurrently against one connection) and `&mut
/// self` for connection lifecycle.
#[async_trait]
pub trait RemoteClient: Send + Sync {
    /// Establish the connection and authenticate.
    async fn connect(&mut self) -> Result<()>;

    /// The connection's default landing directory (the user's home), as an
    /// absolute path — used as the starting directory when a profile has no
    /// explicit remote path, so the user lands somewhere writable rather than at
    /// the filesystem root.
    async fn default_dir(&self) -> Result<RemotePath>;

    /// List the entries of a remote directory.
    ///
    /// Symlinks are reported *as* links ([`EntryKind::Symlink`], lstat-style),
    /// never silently resolved to their target — [`target_kind`] follows a link
    /// on demand.
    ///
    /// [`target_kind`]: RemoteClient::target_kind
    async fn list_dir(&self, path: &RemotePath) -> Result<Vec<RemoteEntry>>;

    /// Recursively walk the remote directory tree rooted at `root`, yielding work
    /// items relative to it in parent-before-child order (see [`DirWalk`]). The
    /// root itself is not emitted — the caller creates the destination root. Used
    /// to plan a recursive download before any bytes move.
    async fn walk_dir(&self, root: &RemotePath) -> Result<DirWalk>;

    /// Follow `path` (resolving a symlink) and report the *target's* kind.
    ///
    /// Used to decide, on click, whether a directory symlink should be navigated
    /// into or treated as a file. One extra round-trip, paid only on activation —
    /// listing stays lstat-cheap. A broken link (missing target) is an error.
    async fn target_kind(&self, path: &RemotePath) -> Result<EntryKind>;

    /// Whether a remote path already exists. Used by the transfer pre-flight gate
    /// to detect an upload that would overwrite an existing destination. A missing
    /// path is `Ok(false)`; only a real error (permissions, transport) is `Err`.
    async fn exists(&self, path: &RemotePath) -> Result<bool>;

    /// Best-effort size of a remote file, for showing a transfer's total up
    /// front. `None` if the size can't be determined — the transfer still runs,
    /// just without a `%`/total. Defaults to `None` for protocols that can't
    /// cheaply stat a file.
    async fn remote_size(&self, path: &RemotePath) -> Option<u64> {
        let _ = path;
        None
    }

    /// Download a remote file to a local path.
    ///
    /// `progress` is bumped per chunk and checked between chunks: a requested
    /// cancellation short-circuits the copy with [`nyx_core::NyxError::Cancelled`].
    async fn download(
        &self,
        remote: &RemotePath,
        local: &Path,
        progress: &TransferProgress,
    ) -> Result<()>;

    /// Upload a local file to a remote path.
    ///
    /// `progress` is bumped per chunk and checked between chunks: a requested
    /// cancellation short-circuits the copy with [`nyx_core::NyxError::Cancelled`].
    async fn upload(
        &self,
        local: &Path,
        remote: &RemotePath,
        progress: &TransferProgress,
    ) -> Result<()>;

    /// Rename / move a remote entry.
    async fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()>;

    /// Delete a remote file or (empty) directory.
    async fn remove(&self, path: &RemotePath) -> Result<()>;

    /// Create a remote directory.
    async fn mkdir(&self, path: &RemotePath) -> Result<()>;

    /// Close the connection cleanly.
    async fn disconnect(&mut self) -> Result<()>;
}
