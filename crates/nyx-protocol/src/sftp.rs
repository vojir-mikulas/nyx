//! SFTP implementation of [`RemoteClient`] over `russh` / `russh-sftp`.
//!
//! The client owns one russh session and one SFTP subsystem channel.
//!
//! **Credential discipline:** the auth secret (password or key passphrase) is
//! held only until [`connect`] uses it and is *never* written to a log or
//! embedded in an error. Auth failures map to the opaque [`NyxError::Auth`] with
//! no server detail echoed back; an encrypted key with a missing/wrong
//! passphrase maps to [`NyxError::KeyLocked`] (also credential-free).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use std::io::SeekFrom;

use nyx_core::{
    EntryKind, FindPredicate, NyxError, Permissions, RemoteEntry, RemotePath, Result,
    ServerTrustKind, SourceMeta, TransferProgress,
};
use russh::client::{self, Handle};
use russh::keys::ssh_key::PublicKey;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg};
use russh::ChannelMsg;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, OpenFlags, StatusCode};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tracing::warn;

use crate::host_key::ServerTrustPrompt;
use crate::known_hosts::{KnownHostStatus, KnownHosts};
use crate::util::{
    copy_counting, map_io_err, open_local_for_resume, plan_removal, plan_walk, RemoveOp,
};
use crate::{DirWalk, RemoteClient};

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
    prompt: Arc<dyn ServerTrustPrompt>,
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
    /// when an unknown host key is presented (see [`ServerTrustPrompt`]).
    pub fn new(
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        auth: Auth,
        known_hosts: KnownHosts,
        prompt: Arc<dyn ServerTrustPrompt>,
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
        // may be a rejected/mismatched host key - surface that precisely.
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

    async fn remote_size(&self, path: &RemotePath) -> Option<u64> {
        self.sftp().ok()?.metadata(path.as_str()).await.ok()?.size
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn remote_meta(&self, path: &RemotePath) -> Option<SourceMeta> {
        let meta = self.sftp().ok()?.metadata(path.as_str()).await.ok()?;
        Some(SourceMeta {
            size: meta.size?,
            mtime: meta.mtime.map(|secs| secs as u64),
        })
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

    async fn walk_dir(&self, root: &RemotePath) -> Result<DirWalk> {
        let sftp = self.sftp()?;
        plan_walk(root.as_str(), move |dir| async move {
            let entries = sftp.read_dir(dir).await.map_err(map_sftp_err)?;
            Ok(entries
                .map(|entry| {
                    let size = entry.metadata().size.unwrap_or(0);
                    (entry.file_name(), map_kind(entry.file_type()), size)
                })
                .collect())
        })
        .await
    }

    /// Server-side search via an SSH `exec` of `find`. One remote command walks
    /// the server's own disk and streams back matched paths - vastly cheaper than
    /// thousands of client `readdir` round-trips. Falls back (`Ok(None)`) when the
    /// server has no shell/`find` or rejects exec, so jailed sftp-only servers
    /// degrade to the client walk. Cancellation is by the caller dropping this
    /// future, which closes the channel and stops `find`.
    async fn server_search(
        &self,
        root: &RemotePath,
        predicates: &[FindPredicate],
        limit: usize,
    ) -> Result<Option<Vec<RemotePath>>> {
        let handle = self
            .handle
            .as_ref()
            .ok_or_else(|| NyxError::Connection("not connected".into()))?;
        let command = build_find_command(root, predicates, limit);

        // A failure to open a channel / run exec means "can't search server-side"
        // (e.g. exec disabled), not a hard error - fall back to the walk.
        let Ok(mut channel) = handle.channel_open_session().await else {
            return Ok(None);
        };
        if channel.exec(true, command.as_bytes()).await.is_err() {
            return Ok(None);
        }

        let mut stdout: Vec<u8> = Vec::new();
        let mut exit_status: Option<u32> = None;
        let mut got_data = false;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    got_data = true;
                    stdout.extend_from_slice(&data);
                    if stdout.len() > MAX_FIND_OUTPUT {
                        // Drop the (possibly half-written, mid-UTF-8) trailing path
                        // so the cap never emits a truncated entry.
                        if let Some(nl) = stdout.iter().rposition(|&b| b == b'\n') {
                            stdout.truncate(nl + 1);
                        }
                        break;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                // Ignore stderr (permission-denied noise is already routed to
                // /dev/null in the command, but be defensive).
                Some(ChannelMsg::ExtendedData { .. }) => {}
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }

        // No output plus a non-zero exit usually means the shell couldn't run
        // `find` (missing, or no shell at all) - fall back rather than report
        // "no matches".
        if !got_data && exit_status.is_some_and(|code| code != 0) {
            return Ok(None);
        }
        Ok(Some(parse_find_output(&stdout, limit)))
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
        offset: u64,
    ) -> Result<()> {
        let sftp = self.sftp()?;
        // The remote-open / local-open halves keep their split mapping; the byte
        // loop itself goes through the `AsyncRead`/`AsyncWrite` interface, which
        // yields `std::io::Error` on either side (mapped via `map_io_err`).
        let mut reader = sftp.open(remote.as_str()).await.map_err(map_sftp_err)?;
        // offset 0 truncates/creates; a resume opens the existing partial,
        // truncates any stale tail past the watermark (so a shrunk source can't
        // leave corrupt trailing bytes) and seeks both ends to it to append.
        let mut writer = if offset == 0 {
            tokio::fs::File::create(local).await.map_err(map_io_err)?
        } else {
            let writer = open_local_for_resume(local, offset).await?;
            reader
                .seek(SeekFrom::Start(offset))
                .await
                .map_err(map_io_err)?;
            writer
        };
        copy_counting(&mut reader, &mut writer, progress).await?;
        writer.flush().await.map_err(map_io_err)?;
        Ok(())
    }

    async fn upload(
        &self,
        local: &Path,
        remote: &RemotePath,
        progress: &TransferProgress,
        offset: u64,
    ) -> Result<()> {
        let sftp = self.sftp()?;
        let mut reader = tokio::fs::File::open(local).await.map_err(map_io_err)?;
        // offset 0 truncates/creates; a resume opens the existing remote partial
        // for writing (no truncate) and seeks both ends to the watermark - the
        // russh-sftp handle does positioned writes from its seek position.
        let mut writer = if offset == 0 {
            sftp.create(remote.as_str()).await.map_err(map_sftp_err)?
        } else {
            let mut writer = sftp
                .open_with_flags(remote.as_str(), OpenFlags::WRITE | OpenFlags::CREATE)
                .await
                .map_err(map_sftp_err)?;
            writer
                .seek(SeekFrom::Start(offset))
                .await
                .map_err(map_io_err)?;
            reader
                .seek(SeekFrom::Start(offset))
                .await
                .map_err(map_io_err)?;
            writer
        };
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

/// The russh client handler - its only job is host-key verification.
struct ClientHandler {
    host: String,
    known_hosts: KnownHosts,
    prompt: Arc<dyn ServerTrustPrompt>,
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
                if self
                    .prompt
                    .confirm_unknown(&self.host, &fingerprint, ServerTrustKind::HostKey)
                    .await
                {
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

/// Hard cap on bytes read from a server search's stdout, so a runaway `find`
/// can't balloon memory even if the `head` cap in the command is absent.
const MAX_FIND_OUTPUT: usize = 4 * 1024 * 1024;

/// Build the `find` command for a server-side search: the root, the AND-combined
/// predicates, stderr silenced (permission-denied noise), and `head` capping the
/// match count. Run by the server's shell, so the pipe is fine.
fn build_find_command(root: &RemotePath, predicates: &[FindPredicate], limit: usize) -> String {
    let mut cmd = format!("find {}", sh_quote(root.as_str()));
    for pred in predicates {
        match pred {
            FindPredicate::Iname(glob) => {
                cmd.push_str(" -iname ");
                cmd.push_str(&sh_quote(glob));
            }
            FindPredicate::Name(glob) => {
                cmd.push_str(" -name ");
                cmd.push_str(&sh_quote(glob));
            }
            FindPredicate::Kind(kind) => {
                cmd.push_str(" -type ");
                cmd.push(match kind {
                    EntryKind::Directory => 'd',
                    EntryKind::Symlink => 'l',
                    EntryKind::File | EntryKind::Other => 'f',
                });
            }
        }
    }
    cmd.push_str(&format!(" 2>/dev/null | head -n {limit}"));
    cmd
}

/// POSIX single-quote a string for safe inclusion in a shell command (the only
/// untrusted inputs are the search root and the user's pattern).
fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Parse `find`'s newline-separated absolute paths, dropping blanks and capping.
fn parse_find_output(bytes: &[u8], limit: usize) -> Vec<RemotePath> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .take(limit)
        .map(RemotePath::new)
        .collect()
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
        // wrong passphrase - re-prompt rather than claim the server refused us.
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

/// Map an SFTP protocol error to [`NyxError`]. The SFTP error `Display` carries
/// only status codes and server messages - no credentials.
///
/// Transport-level failures on an established session (the channel I/O died, a
/// response never came, or the reader task vanished mid-request) map to
/// [`NyxError::ConnectionLost`] so the service can flip the session to "lost".
/// Server `Status` packets are *not* a lost connection - they are ordinary op
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
        // ended (surfaced as an `UnexpectedBehavior` RecvError) - the transport
        // is gone.
        SftpError::IO(_) | SftpError::Timeout | SftpError::UnexpectedBehavior(_) => {
            NyxError::ConnectionLost(err.to_string())
        }
        _ => NyxError::Io(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use super::*;

    #[test]
    fn auth_error_has_no_detail() {
        // The opaque auth error must never carry server/credential detail.
        assert_eq!(NyxError::Auth.to_string(), "authentication failed");
    }

    #[test]
    fn find_command_quotes_and_ands_predicates() {
        let cmd = build_find_command(
            &RemotePath::new("/srv/data"),
            &[
                FindPredicate::Iname("*.rs".into()),
                FindPredicate::Kind(EntryKind::File),
            ],
            5000,
        );
        assert_eq!(
            cmd,
            "find '/srv/data' -iname '*.rs' -type f 2>/dev/null | head -n 5000"
        );
    }

    #[test]
    fn find_command_escapes_quotes_in_paths() {
        // A single quote in the root must not break out of the quoting.
        let cmd = build_find_command(&RemotePath::new("/a'b"), &[], 10);
        assert!(cmd.starts_with("find '/a'\\''b'"), "{cmd}");
    }

    #[test]
    fn parse_find_output_drops_blanks_and_caps() {
        let out = b"/a/x.rs\n/a/y.rs\n\n/a/z.rs\n";
        let paths = parse_find_output(out, 2);
        assert_eq!(
            paths,
            vec![RemotePath::new("/a/x.rs"), RemotePath::new("/a/y.rs")]
        );
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

    /// Drive an async future to completion on a minimal current-thread runtime.
    fn block_on<F: Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(fut)
    }
}
