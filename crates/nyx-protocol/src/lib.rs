// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

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
use nyx_core::{RemoteEntry, Result};

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

    /// List the entries of a remote directory.
    async fn list_dir(&self, path: &str) -> Result<Vec<RemoteEntry>>;

    /// Download a remote file to a local path.
    async fn download(&self, remote: &str, local: &Path) -> Result<()>;

    /// Upload a local file to a remote path.
    async fn upload(&self, local: &Path, remote: &str) -> Result<()>;

    /// Rename / move a remote entry.
    async fn rename(&self, from: &str, to: &str) -> Result<()>;

    /// Delete a remote file or (empty) directory.
    async fn remove(&self, path: &str) -> Result<()>;

    /// Create a remote directory.
    async fn mkdir(&self, path: &str) -> Result<()>;

    /// Close the connection cleanly.
    async fn disconnect(&mut self) -> Result<()>;
}
