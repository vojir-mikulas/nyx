// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! SFTP implementation of [`RemoteClient`] over `russh` / `russh-sftp`.
//!
//! M2 implements `connect` (password auth + host-key verification) and
//! `list_dir`; the remaining file operations land in M4. The client owns one
//! russh session and one SFTP subsystem channel.
//!
//! **Credential discipline:** the password is held only until [`connect`] uses it
//! and is *never* written to a log or embedded in an error. Auth failures map to
//! the opaque [`NyxError::Auth`] with no server detail echoed back.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use nyx_core::{EntryKind, NyxError, RemoteEntry, Result};
use russh::client::{self, Handle};
use russh::keys::ssh_key::PublicKey;
use russh::keys::HashAlg;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, StatusCode};
use tracing::warn;

use crate::host_key::HostKeyPrompt;
use crate::known_hosts::{KnownHostStatus, KnownHosts};
use crate::RemoteClient;

/// An SFTP client (V1 protocol).
///
/// Construct with [`SftpClient::new`], then drive via the [`RemoteClient`] trait.
pub struct SftpClient {
    host: String,
    port: u16,
    username: String,
    /// Held only until [`RemoteClient::connect`] consumes it. Never logged.
    password: String,
    known_hosts: KnownHosts,
    prompt: Arc<dyn HostKeyPrompt>,
    /// Set by the host-key handler when it rejects a key, so [`connect`] can map
    /// the resulting handshake failure to a precise [`NyxError::HostKey`].
    reject_reason: Arc<Mutex<Option<String>>>,
    handle: Option<Handle<ClientHandler>>,
    sftp: Option<SftpSession>,
}

impl SftpClient {
    /// Create a new, unconnected SFTP client.
    ///
    /// `known_hosts` is the trust-on-first-use store and `prompt` is consulted
    /// when an unknown host key is presented (see [`HostKeyPrompt`]).
    pub fn new(
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        password: impl Into<String>,
        known_hosts: KnownHosts,
        prompt: Arc<dyn HostKeyPrompt>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
            password: password.into(),
            known_hosts,
            prompt,
            reject_reason: Arc::new(Mutex::new(None)),
            handle: None,
            sftp: None,
        }
    }

    /// The SFTP subsystem, or a connection error if not connected.
    fn sftp(&self) -> Result<&SftpSession> {
        self.sftp
            .as_ref()
            .ok_or_else(|| NyxError::Connection("not connected".into()))
    }
}

#[async_trait]
impl RemoteClient for SftpClient {
    async fn connect(&mut self) -> Result<()> {
        let config = Arc::new(client::Config::default());
        let handler = ClientHandler {
            host: self.host.clone(),
            known_hosts: self.known_hosts.clone(),
            prompt: self.prompt.clone(),
            reject_reason: self.reject_reason.clone(),
        };

        // Handshake (this is where `check_server_key` runs). A handshake failure
        // may be a rejected/mismatched host key — surface that precisely.
        let mut handle =
            match client::connect(config, (self.host.as_str(), self.port), handler).await {
                Ok(handle) => handle,
                Err(err) => {
                    if let Some(reason) = self.reject_reason.lock().unwrap().take() {
                        return Err(NyxError::HostKey(reason));
                    }
                    return Err(map_russh_err(err));
                }
            };

        // Password auth. Never echo the username/password into the error.
        let result = handle
            .authenticate_password(&self.username, &self.password)
            .await
            .map_err(map_russh_err)?;
        if !result.success() {
            return Err(NyxError::Auth);
        }
        // The password is no longer needed; drop our copy.
        self.password.clear();

        // Open the SFTP subsystem over a session channel.
        let channel = handle.channel_open_session().await.map_err(map_russh_err)?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(map_russh_err)?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(map_sftp_err)?;

        self.handle = Some(handle);
        self.sftp = Some(sftp);
        Ok(())
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<RemoteEntry>> {
        let dir = self.sftp()?.read_dir(path).await.map_err(map_sftp_err)?;
        let mut entries: Vec<RemoteEntry> = Vec::new();
        for item in dir {
            let meta = item.metadata();
            let file_type = item.file_type();
            let is_dir = file_type.is_dir();
            entries.push(RemoteEntry {
                name: item.file_name(),
                size: meta.size.unwrap_or(0),
                kind: map_kind(file_type),
                modified: meta
                    .mtime
                    .map(|secs| UNIX_EPOCH + Duration::from_secs(secs as u64)),
                perms: format_perms(meta.permissions.unwrap_or(0)),
                is_dir,
            });
        }
        Ok(entries)
    }

    async fn download(&self, _remote: &str, _local: &Path) -> Result<()> {
        Err(NyxError::Unsupported)
    }

    async fn upload(&self, _local: &Path, _remote: &str) -> Result<()> {
        Err(NyxError::Unsupported)
    }

    async fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        Err(NyxError::Unsupported)
    }

    async fn remove(&self, _path: &str) -> Result<()> {
        Err(NyxError::Unsupported)
    }

    async fn mkdir(&self, _path: &str) -> Result<()> {
        Err(NyxError::Unsupported)
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Dropping the session + handle closes the channel and SSH connection.
        self.sftp = None;
        self.handle = None;
        Ok(())
    }
}

/// The russh client handler — its only job in M2 is host-key verification.
struct ClientHandler {
    host: String,
    known_hosts: KnownHosts,
    prompt: Arc<dyn HostKeyPrompt>,
    reject_reason: Arc<Mutex<Option<String>>>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        let fingerprint = server_public_key.fingerprint(HashAlg::Sha256).to_string();
        match self.known_hosts.check(&self.host, &fingerprint) {
            KnownHostStatus::Match => Ok(true),
            KnownHostStatus::Mismatch => {
                self.set_reject(format!(
                    "remote host identification has changed for {}",
                    self.host
                ));
                Ok(false)
            }
            KnownHostStatus::Unknown => {
                if self.prompt.confirm_unknown(&self.host, &fingerprint).await {
                    if let Err(err) = self.known_hosts.trust(&self.host, &fingerprint) {
                        warn!(error = %err, "failed to persist trusted host key");
                    }
                    Ok(true)
                } else {
                    self.set_reject("host key rejected".to_string());
                    Ok(false)
                }
            }
        }
    }
}

impl ClientHandler {
    fn set_reject(&self, reason: String) {
        *self.reject_reason.lock().unwrap() = Some(reason);
    }
}

/// Map an SFTP `FileType` to the shared [`EntryKind`].
fn map_kind(file_type: FileType) -> EntryKind {
    match file_type {
        FileType::Dir => EntryKind::Directory,
        FileType::File => EntryKind::File,
        FileType::Symlink => EntryKind::Symlink,
        FileType::Other => EntryKind::Other,
    }
}

/// Render the low 9 bits of a unix mode as `"rwxr-xr-x"`.
fn format_perms(mode: u32) -> String {
    const FLAGS: [(u32, char); 9] = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    FLAGS
        .iter()
        .map(|(bit, ch)| if mode & bit != 0 { *ch } else { '-' })
        .collect()
}

/// Map a `russh` transport error to [`NyxError`], keeping the message
/// credential-free (russh errors never contain the password, but stay coarse).
fn map_russh_err(err: russh::Error) -> NyxError {
    match err {
        russh::Error::NotAuthenticated => NyxError::Auth,
        russh::Error::IO(e) => NyxError::Io(e.to_string()),
        other => NyxError::Connection(other.to_string()),
    }
}

/// Map an SFTP protocol error to [`NyxError`]. The SFTP error `Display` carries
/// only status codes and server messages — no credentials.
fn map_sftp_err(err: russh_sftp::client::error::Error) -> NyxError {
    use russh_sftp::client::error::Error as SftpError;
    match &err {
        SftpError::Status(status) => match status.status_code {
            StatusCode::NoSuchFile => NyxError::NotFound(status.error_message.clone()),
            StatusCode::PermissionDenied => NyxError::Io("permission denied".into()),
            _ => NyxError::Io(err.to_string()),
        },
        _ => NyxError::Io(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perms_render() {
        assert_eq!(format_perms(0o755), "rwxr-xr-x");
        assert_eq!(format_perms(0o644), "rw-r--r--");
        assert_eq!(format_perms(0o600), "rw-------");
        // Type/setuid bits above the low 9 are ignored.
        assert_eq!(format_perms(0o100_644), "rw-r--r--");
    }

    #[test]
    fn auth_error_has_no_detail() {
        // The opaque auth error must never carry server/credential detail.
        assert_eq!(NyxError::Auth.to_string(), "authentication failed");
    }
}
