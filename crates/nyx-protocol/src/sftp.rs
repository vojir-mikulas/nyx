// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! SFTP implementation of [`RemoteClient`].
//!
//! Stub only — the real implementation (built on `russh` / `russh-sftp`) lands
//! in a later plan. Every method currently `unimplemented!()`s so the shape
//! compiles and the service can be wired against it.

use std::path::Path;

use async_trait::async_trait;
use nyx_core::{RemoteEntry, Result};

use crate::RemoteClient;

/// An SFTP client (V1 protocol).
#[derive(Default)]
#[non_exhaustive]
pub struct SftpClient {
    // Connection state (russh session, sftp channel, host config) goes here.
}

impl SftpClient {
    /// Create a new, unconnected SFTP client.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl RemoteClient for SftpClient {
    async fn connect(&mut self) -> Result<()> {
        unimplemented!("SFTP connect is not implemented yet")
    }

    async fn list_dir(&self, _path: &str) -> Result<Vec<RemoteEntry>> {
        unimplemented!("SFTP list_dir is not implemented yet")
    }

    async fn download(&self, _remote: &str, _local: &Path) -> Result<()> {
        unimplemented!("SFTP download is not implemented yet")
    }

    async fn upload(&self, _local: &Path, _remote: &str) -> Result<()> {
        unimplemented!("SFTP upload is not implemented yet")
    }

    async fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        unimplemented!("SFTP rename is not implemented yet")
    }

    async fn remove(&self, _path: &str) -> Result<()> {
        unimplemented!("SFTP remove is not implemented yet")
    }

    async fn mkdir(&self, _path: &str) -> Result<()> {
        unimplemented!("SFTP mkdir is not implemented yet")
    }

    async fn disconnect(&mut self) -> Result<()> {
        unimplemented!("SFTP disconnect is not implemented yet")
    }
}
