//! Protocol abstraction for Nyx.
//!
//! [`RemoteClient`] is the single async trait every protocol implements. V1
//! ships [`SftpClient`]; FTP/FTPS can be added behind the same trait without
//! touching the service or UI layers.
//!
//! The trait is made object-safe with `async_trait` so the service can hold a
//! `Box<dyn RemoteClient>` selected at runtime by [`nyx_core::Protocol`].

use std::path::Path;

use async_trait::async_trait;
use nyx_core::{RemoteEntry, RemotePath, Result, TransferProgress};

mod host_key;
mod known_hosts;
mod sftp;

pub use host_key::HostKeyPrompt;
pub use known_hosts::{KnownHostStatus, KnownHosts};
pub use sftp::SftpClient;

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
    async fn list_dir(&self, path: &RemotePath) -> Result<Vec<RemoteEntry>>;

    /// Whether a remote path already exists. Used by the transfer pre-flight gate
    /// to detect an upload that would overwrite an existing destination. A missing
    /// path is `Ok(false)`; only a real error (permissions, transport) is `Err`.
    async fn exists(&self, path: &RemotePath) -> Result<bool>;

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
