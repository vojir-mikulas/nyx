//! SFTP implementation of [`RemoteClient`] over `russh` / `russh-sftp`.
//!
//! The client owns one russh session and one SFTP subsystem channel.
//!
//! **Credential discipline:** the auth secret (password or key passphrase) is
//! held only until [`connect`] uses it and is *never* written to a log or
//! embedded in an error. Auth failures map to the opaque [`NyxError::Auth`] with
//! no server detail echoed back; an encrypted key with a missing/wrong
//! passphrase maps to [`NyxError::KeyLocked`] (also credential-free).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use nyx_core::{
    EntryKind, NyxError, Permissions, RemoteEntry, RemotePath, Result, TransferProgress,
};
use russh::client::{self, Handle};
use russh::keys::ssh_key::PublicKey;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::warn;

/// The copy-loop chunk size (64 KiB).
const COPY_CHUNK: usize = 64 * 1024;

use crate::host_key::HostKeyPrompt;
use crate::known_hosts::{KnownHostStatus, KnownHosts};
use crate::RemoteClient;

/// How a client proves its identity to the server.
///
/// The contained secret (the password, or a key's passphrase) is held only until
/// [`RemoteClient::connect`] consumes it and is never logged. The key *path* is
/// not a secret.
pub enum Auth {
    /// Password authentication.
    Password(String),
    /// Public-key authentication with an OpenSSH private key file.
    Key {
        /// Path to the private key file.
        path: PathBuf,
        /// Passphrase for an encrypted key; `None` (or empty) for an
        /// unencrypted key.
        passphrase: Option<String>,
    },
}

/// An SFTP client (V1 protocol).
///
/// Construct with [`SftpClient::new`], then drive via the [`RemoteClient`] trait.
pub struct SftpClient {
    host: String,
    port: u16,
    username: String,
    /// The chosen auth method + secret. Held only until [`RemoteClient::connect`]
    /// consumes it, then cleared. Never logged.
    auth: Auth,
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
        auth: Auth,
        known_hosts: KnownHosts,
        prompt: Arc<dyn HostKeyPrompt>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
            auth,
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
    /// front. `None` if the stat fails or the size is unknown — the transfer
    /// still runs, just without a `%`/total.
    pub async fn remote_size(&self, path: &RemotePath) -> Option<u64> {
        self.sftp().ok()?.metadata(path.as_str()).await.ok()?.size
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

        // Authenticate by the chosen method. Never echo the username or any
        // secret into an error.
        let result = match &self.auth {
            Auth::Password(password) => handle
                .authenticate_password(&self.username, password)
                .await
                .map_err(map_russh_err)?,
            Auth::Key { path, passphrase } => {
                let key = load_private_key(path, passphrase.as_deref()).await?;
                handle
                    .authenticate_publickey(&self.username, key)
                    .await
                    .map_err(map_russh_err)?
            }
        };
        if !result.success() {
            return Err(NyxError::Auth);
        }
        // The secret is no longer needed; drop our copy.
        self.auth = Auth::Password(String::new());

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

    async fn default_dir(&self) -> Result<RemotePath> {
        // `canonicalize(".")` resolves the SFTP session's start directory (the
        // user's home on most servers) to an absolute path.
        let dir = self.sftp()?.canonicalize(".").await.map_err(map_sftp_err)?;
        Ok(RemotePath::new(dir))
    }

    async fn exists(&self, path: &RemotePath) -> Result<bool> {
        match self.sftp()?.metadata(path.as_str()).await {
            Ok(_) => Ok(true),
            // A missing file is the expected "no collision" answer, not an error.
            Err(russh_sftp::client::error::Error::Status(status))
                if status.status_code == StatusCode::NoSuchFile =>
            {
                Ok(false)
            }
            Err(err) => Err(map_sftp_err(err)),
        }
    }

    async fn list_dir(&self, path: &RemotePath) -> Result<Vec<RemoteEntry>> {
        let dir = self
            .sftp()?
            .read_dir(path.as_str())
            .await
            .map_err(map_sftp_err)?;
        let mut entries: Vec<RemoteEntry> = Vec::new();
        for item in dir {
            let meta = item.metadata();
            entries.push(RemoteEntry {
                name: item.file_name(),
                size: meta.size.unwrap_or(0),
                kind: map_kind(item.file_type()),
                modified: meta
                    .mtime
                    .map(|secs| UNIX_EPOCH + Duration::from_secs(secs as u64)),
                permissions: Permissions::from_mode(meta.permissions.unwrap_or(0)),
            });
        }
        Ok(entries)
    }

    async fn target_kind(&self, path: &RemotePath) -> Result<EntryKind> {
        // `metadata` is a follow-stat (`SSH_FXP_STAT`), so a symlink resolves to
        // its target here; `list_dir` uses the lstat-style readdir attrs instead.
        let meta = self
            .sftp()?
            .metadata(path.as_str())
            .await
            .map_err(map_sftp_err)?;
        Ok(map_kind(meta.file_type()))
    }

    async fn download(
        &self,
        remote: &RemotePath,
        local: &Path,
        progress: &TransferProgress,
    ) -> Result<()> {
        let sftp = self.sftp()?;
        // The remote-open / local-create halves keep their split mapping; the
        // byte loop itself goes through the `AsyncRead`/`AsyncWrite` interface,
        // which yields `std::io::Error` on either side (mapped via `map_io_err`).
        let mut reader = sftp.open(remote.as_str()).await.map_err(map_sftp_err)?;
        let mut writer = tokio::fs::File::create(local).await.map_err(map_io_err)?;
        copy_counting(&mut reader, &mut writer, progress).await?;
        writer.flush().await.map_err(map_io_err)?;
        Ok(())
    }

    async fn upload(
        &self,
        local: &Path,
        remote: &RemotePath,
        progress: &TransferProgress,
    ) -> Result<()> {
        let sftp = self.sftp()?;
        let mut reader = tokio::fs::File::open(local).await.map_err(map_io_err)?;
        let mut writer = sftp.create(remote.as_str()).await.map_err(map_sftp_err)?;
        copy_counting(&mut reader, &mut writer, progress).await?;
        // Flush + close the remote handle so all queued writes are acknowledged
        // before we report success.
        writer.shutdown().await.map_err(map_io_err)?;
        Ok(())
    }

    async fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        self.sftp()?
            .rename(from.as_str(), to.as_str())
            .await
            .map_err(map_sftp_err)
    }

    async fn remove(&self, path: &RemotePath) -> Result<()> {
        let sftp = self.sftp()?;
        // SFTP's `remove_dir` only deletes an *empty* directory, so a directory
        // target is removed depth-first (children before parent). The traversal
        // is planned with an explicit work-stack (no boxed async recursion).
        let path = path.as_str();
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

    async fn mkdir(&self, path: &RemotePath) -> Result<()> {
        self.sftp()?
            .create_dir(path.as_str())
            .await
            .map_err(map_sftp_err)
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Dropping the session + handle closes the channel and SSH connection.
        self.sftp = None;
        self.handle = None;
        Ok(())
    }
}

/// The russh client handler — its only job is host-key verification.
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

/// Load + decrypt an OpenSSH private key for public-key auth.
///
/// The decode (and, for an encrypted key, the bcrypt KDF) is CPU-bound and reads
/// the file synchronously, so it runs on a blocking thread to avoid stalling the
/// async runtime. An empty passphrase is treated as "none" (an unencrypted key).
/// Errors are mapped credential-free by [`map_key_load_err`].
async fn load_private_key(path: &Path, passphrase: Option<&str>) -> Result<PrivateKeyWithHashAlg> {
    let path = path.to_path_buf();
    let passphrase = passphrase.filter(|p| !p.is_empty()).map(str::to_string);
    let had_passphrase = passphrase.is_some();
    let key = tokio::task::spawn_blocking(move || {
        russh::keys::load_secret_key(&path, passphrase.as_deref())
            .map_err(|err| map_key_load_err(err, &path, had_passphrase))
    })
    .await
    .map_err(|err| NyxError::Other(err.to_string()))??;

    // RSA keys must be signed with a modern hash; the legacy ssh-rsa (SHA-1) is
    // rejected by current servers. `PrivateKeyWithHashAlg::new` ignores the hash
    // for non-RSA keys.
    let hash_alg = key.algorithm().is_rsa().then_some(HashAlg::Sha512);
    Ok(PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg))
}

/// Map a private-key load error to a credential-free [`NyxError`].
///
/// An encrypted key with a missing or wrong passphrase becomes
/// [`NyxError::KeyLocked`] so the UI re-prompts; a missing file becomes
/// [`NyxError::NotFound`] (the path is not a secret). The russh key error
/// `Display` never contains key bytes, but we still only surface coarse detail.
fn map_key_load_err(err: russh::keys::Error, path: &Path, had_passphrase: bool) -> NyxError {
    use russh::keys::Error as KeyError;
    match err {
        // Encrypted key, no passphrase supplied.
        KeyError::KeyIsEncrypted => NyxError::KeyLocked,
        KeyError::IO(e) if e.kind() == std::io::ErrorKind::NotFound => {
            NyxError::NotFound(path.display().to_string())
        }
        KeyError::IO(e) => NyxError::Io(e.to_string()),
        // A decode failure once a passphrase *was* supplied is, in practice, a
        // wrong passphrase — re-prompt rather than claim the server refused us.
        _ if had_passphrase => NyxError::KeyLocked,
        other => NyxError::Io(format!("invalid private key: {other}")),
    }
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
///
/// Transport-level failures on an established session (the channel I/O died, a
/// response never came, or the reader task vanished mid-request) map to
/// [`NyxError::ConnectionLost`] so the service can flip the session to "lost".
/// Server `Status` packets are *not* a lost connection — they are ordinary op
/// failures (missing file, permission denied, …).
fn map_sftp_err(err: russh_sftp::client::error::Error) -> NyxError {
    use russh_sftp::client::error::Error as SftpError;
    match &err {
        SftpError::Status(status) => match status.status_code {
            StatusCode::NoSuchFile => NyxError::NotFound(status.error_message.clone()),
            StatusCode::PermissionDenied => NyxError::Io("permission denied".into()),
            _ => NyxError::Io(err.to_string()),
        },
        // Channel I/O died, the response timed out, or the session's reader task
        // ended (surfaced as an `UnexpectedBehavior` RecvError) — the transport
        // is gone.
        SftpError::IO(_) | SftpError::Timeout | SftpError::UnexpectedBehavior(_) => {
            NyxError::ConnectionLost(err.to_string())
        }
        _ => NyxError::Io(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_error_has_no_detail() {
        // The opaque auth error must never carry server/credential detail.
        assert_eq!(NyxError::Auth.to_string(), "authentication failed");
    }

    #[test]
    fn map_kind_distinguishes_symlinks() {
        assert_eq!(map_kind(FileType::Symlink), EntryKind::Symlink);
        assert_eq!(map_kind(FileType::Dir), EntryKind::Directory);
        assert_eq!(map_kind(FileType::File), EntryKind::File);
        assert_eq!(map_kind(FileType::Other), EntryKind::Other);
    }

    #[test]
    fn transport_failures_map_to_connection_lost() {
        use russh_sftp::client::error::Error as SftpError;
        // A dead channel, a timed-out response, or a vanished reader task all mean
        // the established transport is gone.
        assert!(matches!(
            map_sftp_err(SftpError::IO("broken pipe".into())),
            NyxError::ConnectionLost(_)
        ));
        assert!(matches!(
            map_sftp_err(SftpError::Timeout),
            NyxError::ConnectionLost(_)
        ));
        assert!(matches!(
            map_sftp_err(SftpError::UnexpectedBehavior("RecvError".into())),
            NyxError::ConnectionLost(_)
        ));
    }

    #[test]
    fn server_status_is_not_a_lost_connection() {
        use russh_sftp::client::error::Error as SftpError;
        use russh_sftp::protocol::Status;
        let no_such = SftpError::Status(Status {
            id: 1,
            status_code: StatusCode::NoSuchFile,
            error_message: "no such file".into(),
            language_tag: String::new(),
        });
        assert!(matches!(map_sftp_err(no_such), NyxError::NotFound(_)));
    }

    /// An encrypted Ed25519 key (OpenSSH format); the passphrase is `blabla`.
    const ENCRYPTED_ED25519: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jYmMAAAAGYmNyeXB0AAAAGAAAABDLGyfA39
J2FcJygtYqi5ISAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIN+Wjn4+4Fcvl2Jl
KpggT+wCRxpSvtqqpVrQrKN1/A22AAAAkOHDLnYZvYS6H9Q3S3Nk4ri3R2jAZlQlBbUos5
FkHpYgNw65KCWCTXtP7ye2czMC3zjn2r98pJLobsLYQgRiHIv/CUdAdsqbvMPECB+wl/UQ
e+JpiSq66Z6GIt0801skPh20jxOO3F52SoX1IeO5D5PXfZrfSZlw6S8c7bwyp2FHxDewRx
7/wNsnDM0T7nLv/Q==
-----END OPENSSH PRIVATE KEY-----";

    /// An unencrypted Ed25519 key (RFC 8410 PKCS#8 form).
    const UNENCRYPTED_ED25519: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEINTuctv5E1hK1bbY8fdp+K06/nwoy/HU++CXqI9EdVhC
-----END PRIVATE KEY-----";

    fn key_file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_test");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn unencrypted_key_loads_without_passphrase() {
        let (_dir, path) = key_file(UNENCRYPTED_ED25519);
        assert!(block_on(load_private_key(&path, None)).is_ok());
    }

    #[test]
    fn encrypted_key_with_correct_passphrase_loads() {
        let (_dir, path) = key_file(ENCRYPTED_ED25519);
        assert!(block_on(load_private_key(&path, Some("blabla"))).is_ok());
    }

    #[test]
    fn encrypted_key_with_no_passphrase_is_key_locked() {
        let (_dir, path) = key_file(ENCRYPTED_ED25519);
        let err = block_on(load_private_key(&path, None)).unwrap_err();
        assert!(matches!(err, NyxError::KeyLocked));
        assert_eq!(err.to_string(), "key requires a passphrase");
    }

    #[test]
    fn encrypted_key_with_wrong_passphrase_is_key_locked() {
        let (_dir, path) = key_file(ENCRYPTED_ED25519);
        let err = block_on(load_private_key(&path, Some("nope"))).unwrap_err();
        assert!(matches!(err, NyxError::KeyLocked));
        // The error must never echo the (wrong) passphrase.
        assert!(!err.to_string().contains("nope"));
    }

    #[test]
    fn missing_key_file_is_not_found() {
        let path = std::path::Path::new("/no/such/key/id_ed25519");
        let err = block_on(load_private_key(path, None)).unwrap_err();
        assert!(matches!(err, NyxError::NotFound(_)));
        // The path is not a secret, so it is fine to surface.
        assert!(err.to_string().contains("id_ed25519"));
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
