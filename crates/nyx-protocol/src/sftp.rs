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

use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use nyx_core::{EntryKind, NyxError, RemoteEntry, Result, TransferProgress};
use russh::client::{self, Handle};
use russh::keys::ssh_key::PublicKey;
use russh::keys::HashAlg;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::warn;

/// The copy-loop chunk size (64 KiB).
const COPY_CHUNK: usize = 64 * 1024;

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

    /// Best-effort size of a remote file, for showing a transfer's total up
    /// front (M5, D6). `None` if the stat fails or the size is unknown — the
    /// transfer still runs, just without a `%`/total.
    pub async fn remote_size(&self, path: &str) -> Option<u64> {
        self.sftp().ok()?.metadata(path).await.ok()?.size
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

    async fn download(
        &self,
        remote: &str,
        local: &Path,
        progress: &TransferProgress,
    ) -> Result<()> {
        let sftp = self.sftp()?;
        // The remote-open / local-create halves keep their split mapping; the
        // byte loop itself goes through the `AsyncRead`/`AsyncWrite` interface,
        // which yields `std::io::Error` on either side (mapped via `map_io_err`).
        let mut reader = sftp.open(remote).await.map_err(map_sftp_err)?;
        let mut writer = tokio::fs::File::create(local).await.map_err(map_io_err)?;
        copy_counting(&mut reader, &mut writer, progress).await?;
        writer.flush().await.map_err(map_io_err)?;
        Ok(())
    }

    async fn upload(&self, local: &Path, remote: &str, progress: &TransferProgress) -> Result<()> {
        let sftp = self.sftp()?;
        let mut reader = tokio::fs::File::open(local).await.map_err(map_io_err)?;
        let mut writer = sftp.create(remote).await.map_err(map_sftp_err)?;
        copy_counting(&mut reader, &mut writer, progress).await?;
        // Flush + close the remote handle so all queued writes are acknowledged
        // before we report success.
        writer.shutdown().await.map_err(map_io_err)?;
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.sftp()?.rename(from, to).await.map_err(map_sftp_err)
    }

    async fn remove(&self, path: &str) -> Result<()> {
        let sftp = self.sftp()?;
        // SFTP's `remove_dir` only deletes an *empty* directory, so a directory
        // target is removed depth-first (children before parent). The traversal
        // is planned with an explicit work-stack (no boxed async recursion).
        let is_dir = sftp.metadata(path).await.map_err(map_sftp_err)?.is_dir();
        let ops = plan_removal(path, is_dir, move |dir| async move {
            let entries = sftp.read_dir(dir).await.map_err(map_sftp_err)?;
            Ok(entries
                .map(|entry| (entry.path(), entry.file_type().is_dir()))
                .collect())
        })
        .await?;
        for op in ops {
            match op {
                RemoveOp::File(p) => sftp.remove_file(p).await.map_err(map_sftp_err)?,
                RemoveOp::Dir(p) => sftp.remove_dir(p).await.map_err(map_sftp_err)?,
            }
        }
        Ok(())
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        self.sftp()?.create_dir(path).await.map_err(map_sftp_err)
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

/// Copy `reader` → `writer` in 64 KiB chunks, bumping `progress` per chunk and
/// checking for a requested cancel between chunks.
///
/// Both halves are driven through tokio's `AsyncRead`/`AsyncWrite`, which surface
/// `std::io::Error` regardless of which side (remote SFTP handle or local file)
/// errors — hence the single [`map_io_err`]. A cancel short-circuits with
/// [`NyxError::Cancelled`]; the caller (service) does any partial-file cleanup.
async fn copy_counting<R, W>(
    reader: &mut R,
    writer: &mut W,
    progress: &TransferProgress,
) -> Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buf = vec![0u8; COPY_CHUNK];
    loop {
        if progress.is_cancelled() {
            return Err(NyxError::Cancelled);
        }
        let n = reader.read(&mut buf).await.map_err(map_io_err)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await.map_err(map_io_err)?;
        progress.add(n as u64);
    }
    Ok(())
}

/// Map a **local** filesystem / transfer-copy error to [`NyxError`]. Used for
/// the local half of a download (write) or upload (read); paths aren't secrets,
/// but the message stays coarse (an OS error string, never a credential).
fn map_io_err(err: std::io::Error) -> NyxError {
    NyxError::Io(err.to_string())
}

/// One step of a recursive removal, in the order it must be performed: a
/// directory only ever appears **after** all of its descendants.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoveOp {
    /// Delete a file (or symlink) at this absolute path.
    File(String),
    /// Delete a now-empty directory at this absolute path.
    Dir(String),
}

/// Plan the depth-first removal of `root` without async recursion.
///
/// A file target yields a single [`RemoveOp::File`]. A directory target is
/// walked with an explicit work-stack: each directory is visited twice — once to
/// list its children (pushing sub-directories back on the stack and emitting its
/// files), and once (after its children) to emit the directory's own
/// [`RemoveOp::Dir`]. `list_dir` yields each directory's `(path, is_dir)`
/// children. The result is post-order, so applying it in sequence removes the
/// whole tree leaf-first.
async fn plan_removal<F, Fut>(
    root: &str,
    root_is_dir: bool,
    mut list_dir: F,
) -> Result<Vec<RemoveOp>>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Vec<(String, bool)>>>,
{
    if !root_is_dir {
        return Ok(vec![RemoveOp::File(root.to_string())]);
    }
    let mut ops = Vec::new();
    // (path, expanded): an unexpanded directory still needs listing; an expanded
    // one has had its children queued and is ready to be removed.
    let mut stack: Vec<(String, bool)> = vec![(root.to_string(), false)];
    while let Some((path, expanded)) = stack.pop() {
        if expanded {
            ops.push(RemoveOp::Dir(path));
            continue;
        }
        let children = list_dir(path.clone()).await?;
        // Re-push this directory below its children so it is removed last.
        stack.push((path, true));
        for (child, is_dir) in children {
            if is_dir {
                stack.push((child, false));
            } else {
                ops.push(RemoveOp::File(child));
            }
        }
    }
    Ok(ops)
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

    /// Drive an async future to completion on a minimal current-thread runtime
    /// (the recursive-remove traversal is async but server-free in the test).
    fn block_on<F: Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(fut)
    }

    #[test]
    fn removal_of_a_file_is_a_single_op() {
        let ops = block_on(plan_removal("/srv/report.pdf", false, |_| async {
            unreachable!("a file target is never listed")
        }))
        .unwrap();
        assert_eq!(ops, vec![RemoveOp::File("/srv/report.pdf".into())]);
    }

    #[test]
    fn removal_of_a_tree_is_post_order() {
        use std::collections::HashMap;

        // /root
        //   a.txt
        //   sub/
        //     c.txt
        //     deep/
        //       d.txt
        //   b.txt
        let tree: HashMap<&str, Vec<(&str, bool)>> = HashMap::from([
            (
                "/root",
                vec![
                    ("/root/a.txt", false),
                    ("/root/sub", true),
                    ("/root/b.txt", false),
                ],
            ),
            (
                "/root/sub",
                vec![("/root/sub/c.txt", false), ("/root/sub/deep", true)],
            ),
            ("/root/sub/deep", vec![("/root/sub/deep/d.txt", false)]),
        ]);

        let ops = block_on(plan_removal("/root", true, |dir| {
            let tree = &tree;
            async move {
                Ok(tree
                    .get(dir.as_str())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(p, is_dir)| (p.to_string(), is_dir))
                    .collect())
            }
        }))
        .unwrap();

        // Every file before its parent dir; every dir after all its descendants.
        assert_eq!(
            ops,
            vec![
                RemoveOp::File("/root/a.txt".into()),
                RemoveOp::File("/root/b.txt".into()),
                RemoveOp::File("/root/sub/c.txt".into()),
                RemoveOp::File("/root/sub/deep/d.txt".into()),
                RemoveOp::Dir("/root/sub/deep".into()),
                RemoveOp::Dir("/root/sub".into()),
                RemoveOp::Dir("/root".into()),
            ]
        );
    }
}
