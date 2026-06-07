//! Plain-FTP implementation of [`RemoteClient`] over `suppaftp` (tokio backend).
//!
//! FTP keeps a single stateful control connection and opens one data connection
//! at a time, so the [`suppaftp`] stream is held behind a [`tokio::sync::Mutex`]
//! and every operation serializes over it — there is no safe concurrency on one
//! FTP connection. The protocol-level command logic (listing, walking, transfers,
//! removal) is written **generically** over the suppaftp stream type so the
//! [`FtpsClient`](crate::FtpsClient) reuses it verbatim over a TLS stream.
//!
//! **Credential discipline:** the password is held only until [`connect`] logs in,
//! then cleared, and is never logged or embedded in an error. We mark FTP as
//! insecure in the UI, but never leak the secret ourselves.
//!
//! [`connect`]: RemoteClient::connect

use std::path::Path;

use async_trait::async_trait;
use nyx_core::{
    EntryIssue, EntryKind, NyxError, Permissions, RemoteEntry, RemotePath, Result, TransferProgress,
};
use suppaftp::list::{File, PosixPexQuery};
use suppaftp::tokio::{AsyncFtpStream, ImplAsyncFtpStream, TokioTlsStream};
use suppaftp::types::FileType;
use suppaftp::{FtpError, Mode};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::util::{copy_counting, map_io_err, reject_offset, RemoveOp};
use crate::{DirWalk, RemoteClient, WalkItem};

/// A plain-FTP client.
///
/// Construct with [`FtpClient::new`], then drive via the [`RemoteClient`] trait.
pub struct FtpClient {
    host: String,
    port: u16,
    username: String,
    /// The login password. Held only until [`RemoteClient::connect`] consumes it,
    /// then cleared. Never logged.
    password: String,
    /// The live control connection, behind a mutex so the trait's `&self` methods
    /// can serialize over the one connection.
    stream: Mutex<Option<AsyncFtpStream>>,
}

impl FtpClient {
    /// Create a new, unconnected plain-FTP client.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
            password: password.into(),
            stream: Mutex::new(None),
        }
    }
}

#[async_trait]
impl RemoteClient for FtpClient {
    async fn connect(&mut self) -> Result<()> {
        let addr = format!("{}:{}", self.host, self.port);
        let mut stream = AsyncFtpStream::connect(addr)
            .await
            .map_err(map_connect_err)?;
        stream
            .login(self.username.as_str(), self.password.as_str())
            .await
            .map_err(map_ftp_err)?;
        // The secret is no longer needed; drop our copy.
        self.password.clear();
        op_setup(&mut stream).await?;
        *self.stream.lock().await = Some(stream);
        Ok(())
    }

    async fn default_dir(&self) -> Result<RemotePath> {
        let mut guard = self.stream.lock().await;
        op_default_dir(stream_mut(&mut guard)?).await
    }

    async fn list_dir(&self, path: &RemotePath) -> Result<Vec<RemoteEntry>> {
        let mut guard = self.stream.lock().await;
        op_list_dir(stream_mut(&mut guard)?, path).await
    }

    async fn walk_dir(&self, root: &RemotePath) -> Result<DirWalk> {
        let mut guard = self.stream.lock().await;
        op_walk_dir(stream_mut(&mut guard)?, root).await
    }

    async fn target_kind(&self, path: &RemotePath) -> Result<EntryKind> {
        let mut guard = self.stream.lock().await;
        op_target_kind(stream_mut(&mut guard)?, path).await
    }

    async fn exists(&self, path: &RemotePath) -> Result<bool> {
        let mut guard = self.stream.lock().await;
        op_exists(stream_mut(&mut guard)?, path).await
    }

    async fn remote_size(&self, path: &RemotePath) -> Option<u64> {
        let mut guard = self.stream.lock().await;
        op_remote_size(guard.as_mut()?, path).await
    }

    async fn download(
        &self,
        remote: &RemotePath,
        local: &Path,
        progress: &TransferProgress,
        offset: u64,
    ) -> Result<()> {
        // FTP resume (REST) is a follow-up; `supports_resume` is false, so a
        // non-zero offset never reaches here — reject it defensively rather than
        // silently corrupt by ignoring it.
        reject_offset(offset)?;
        let mut guard = self.stream.lock().await;
        op_download(stream_mut(&mut guard)?, remote, local, progress).await
    }

    async fn upload(
        &self,
        local: &Path,
        remote: &RemotePath,
        progress: &TransferProgress,
        offset: u64,
    ) -> Result<()> {
        reject_offset(offset)?;
        let mut guard = self.stream.lock().await;
        op_upload(stream_mut(&mut guard)?, local, remote, progress).await
    }

    async fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        let mut guard = self.stream.lock().await;
        stream_mut(&mut guard)?
            .rename(from.as_str(), to.as_str())
            .await
            .map_err(map_ftp_err)
    }

    async fn remove(&self, path: &RemotePath) -> Result<()> {
        let mut guard = self.stream.lock().await;
        op_remove(stream_mut(&mut guard)?, path).await
    }

    async fn mkdir(&self, path: &RemotePath) -> Result<()> {
        let mut guard = self.stream.lock().await;
        stream_mut(&mut guard)?
            .mkdir(path.as_str())
            .await
            .map_err(map_ftp_err)
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(mut stream) = self.stream.lock().await.take() {
            // Best-effort QUIT; dropping the stream closes the socket regardless.
            let _ = stream.quit().await;
        }
        Ok(())
    }
}

/// The live stream out of a held lock guard, or a connection error if not
/// connected.
fn stream_mut<T>(guard: &mut Option<ImplAsyncFtpStream<T>>) -> Result<&mut ImplAsyncFtpStream<T>>
where
    T: TokioTlsStream + Send,
{
    guard
        .as_mut()
        .ok_or_else(|| NyxError::Connection("not connected".into()))
}

// --- Generic, stream-agnostic FTP/FTPS operations -------------------------------
//
// Written over the suppaftp stream type `T` so plain FTP and FTPS share one
// implementation. The `'static` bound is required by suppaftp's `abort`, which we
// use to tear down a data connection on cancel.

/// Put the connection in passive mode and binary (`TYPE I`) transfers — the
/// defaults every other op assumes (passive for NAT-friendliness, binary so
/// sizes and bytes are exact).
pub(crate) async fn op_setup<T>(stream: &mut ImplAsyncFtpStream<T>) -> Result<()>
where
    T: TokioTlsStream + Send + 'static,
{
    stream.set_mode(Mode::Passive);
    stream
        .transfer_type(FileType::Binary)
        .await
        .map_err(map_ftp_err)
}

/// The login landing directory (`PWD`).
pub(crate) async fn op_default_dir<T>(stream: &mut ImplAsyncFtpStream<T>) -> Result<RemotePath>
where
    T: TokioTlsStream + Send + 'static,
{
    let dir = stream.pwd().await.map_err(map_ftp_err)?;
    Ok(RemotePath::new(dir))
}

/// List a directory, preferring the structured `MLSD` and falling back to `LIST`
/// for servers without it. `.`/`..` are filtered out.
pub(crate) async fn op_list_dir<T>(
    stream: &mut ImplAsyncFtpStream<T>,
    path: &RemotePath,
) -> Result<Vec<RemoteEntry>>
where
    T: TokioTlsStream + Send + 'static,
{
    let p = path.as_str();
    let lines = match stream.mlsd(Some(p)).await {
        Ok(lines) if !lines.is_empty() => lines,
        // An MLSD that returns nothing is an empty directory.
        Ok(_) => Vec::new(),
        Err(err) if is_not_found(&err) => return Err(map_ftp_err(err)),
        // MLSD unsupported (or otherwise refused) — fall back to LIST.
        Err(_) => stream.list(Some(p)).await.map_err(map_ftp_err)?,
    };
    let mut entries = Vec::with_capacity(lines.len());
    for line in lines {
        // Heterogeneous servers vary the line format; skip anything unparseable
        // rather than failing the whole listing.
        let Ok(file) = File::try_from(line.as_str()) else {
            continue;
        };
        let name = file.name().to_string();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        entries.push(RemoteEntry {
            name,
            size: file.size() as u64,
            kind: file_kind(&file),
            modified: Some(file.modified()),
            permissions: Permissions::from_mode(perm_mode(&file)),
        });
    }
    Ok(entries)
}

/// Recursively walk `root` pre-order (parent before children), reusing
/// [`op_list_dir`]. FTP has no native recursive list, so this drives an explicit
/// stack — no async recursion. Symlinks/specials are skipped and tallied.
pub(crate) async fn op_walk_dir<T>(
    stream: &mut ImplAsyncFtpStream<T>,
    root: &RemotePath,
) -> Result<DirWalk>
where
    T: TokioTlsStream + Send + 'static,
{
    let mut walk = DirWalk::default();
    let mut stack: Vec<(String, Vec<String>)> = vec![(root.as_str().to_string(), Vec::new())];
    while let Some((dir, rel)) = stack.pop() {
        let entries = op_list_dir(stream, &RemotePath::new(&dir)).await?;
        for entry in entries {
            let mut child_rel = rel.clone();
            child_rel.push(entry.name.clone());
            let child_abs = format!("{dir}/{}", entry.name);
            match entry.kind {
                EntryKind::Directory => {
                    walk.items.push(WalkItem {
                        rel: child_rel.clone(),
                        is_dir: true,
                        size: 0,
                    });
                    stack.push((child_abs, child_rel));
                }
                EntryKind::File => {
                    walk.total_bytes += entry.size;
                    walk.items.push(WalkItem {
                        rel: child_rel,
                        is_dir: false,
                        size: entry.size,
                    });
                }
                EntryKind::Symlink => walk
                    .skips
                    .push(EntryIssue::skipped(child_rel.join("/"), "symlink skipped")),
                EntryKind::Other => walk.skips.push(EntryIssue::skipped(
                    child_rel.join("/"),
                    "special file skipped",
                )),
            }
        }
    }
    Ok(walk)
}

/// Report a path's kind (FTP has no symlink-follow semantics, so the listed kind
/// is the answer). A missing path is [`NyxError::NotFound`].
pub(crate) async fn op_target_kind<T>(
    stream: &mut ImplAsyncFtpStream<T>,
    path: &RemotePath,
) -> Result<EntryKind>
where
    T: TokioTlsStream + Send + 'static,
{
    op_kind(stream, path)
        .await?
        .ok_or_else(|| NyxError::NotFound(path.as_str().to_string()))
}

/// Whether a path exists (file or directory).
pub(crate) async fn op_exists<T>(
    stream: &mut ImplAsyncFtpStream<T>,
    path: &RemotePath,
) -> Result<bool>
where
    T: TokioTlsStream + Send + 'static,
{
    Ok(op_kind(stream, path).await?.is_some())
}

/// Best-effort file size via `SIZE` (binary mode). `None` on any failure.
pub(crate) async fn op_remote_size<T>(
    stream: &mut ImplAsyncFtpStream<T>,
    path: &RemotePath,
) -> Option<u64>
where
    T: TokioTlsStream + Send + 'static,
{
    stream.size(path.as_str()).await.ok().map(|s| s as u64)
}

/// Download `remote` → `local`, streaming through [`copy_counting`]. On cancel the
/// data connection is `abort`ed to keep the control channel in sync.
pub(crate) async fn op_download<T>(
    stream: &mut ImplAsyncFtpStream<T>,
    remote: &RemotePath,
    local: &Path,
    progress: &TransferProgress,
) -> Result<()>
where
    T: TokioTlsStream + Send + 'static,
{
    let mut reader = stream
        .retr_as_stream(remote.as_str())
        .await
        .map_err(map_ftp_err)?;
    let mut writer = tokio::fs::File::create(local).await.map_err(map_io_err)?;
    let copy_res = copy_counting(&mut reader, &mut writer, progress).await;
    match &copy_res {
        Ok(()) => {
            writer.flush().await.map_err(map_io_err)?;
            stream
                .finalize_retr_stream(reader)
                .await
                .map_err(map_ftp_err)?;
        }
        Err(_) => {
            let _ = stream.abort(reader).await;
        }
    }
    copy_res
}

/// Upload `local` → `remote`, streaming through [`copy_counting`]. On cancel the
/// data connection is `abort`ed to keep the control channel in sync.
pub(crate) async fn op_upload<T>(
    stream: &mut ImplAsyncFtpStream<T>,
    local: &Path,
    remote: &RemotePath,
    progress: &TransferProgress,
) -> Result<()>
where
    T: TokioTlsStream + Send + 'static,
{
    let mut reader = tokio::fs::File::open(local).await.map_err(map_io_err)?;
    let mut writer = stream
        .put_with_stream(remote.as_str())
        .await
        .map_err(map_ftp_err)?;
    let copy_res = copy_counting(&mut reader, &mut writer, progress).await;
    match &copy_res {
        Ok(()) => {
            stream
                .finalize_put_stream(writer)
                .await
                .map_err(map_ftp_err)?;
        }
        Err(_) => {
            let _ = stream.abort(writer).await;
        }
    }
    copy_res
}

/// Delete a file or (recursively) a directory. FTP's `RMD` only removes an empty
/// directory, so a directory target is deleted leaf-first via an explicit
/// post-order plan.
pub(crate) async fn op_remove<T>(
    stream: &mut ImplAsyncFtpStream<T>,
    path: &RemotePath,
) -> Result<()>
where
    T: TokioTlsStream + Send + 'static,
{
    let is_dir = matches!(op_kind(stream, path).await?, Some(EntryKind::Directory));
    if !is_dir {
        return stream.rm(path.as_str()).await.map_err(map_ftp_err);
    }
    let ops = plan_ftp_removal(stream, path.as_str()).await?;
    for op in ops {
        match op {
            RemoveOp::File(p) => stream.rm(p).await.map_err(map_ftp_err)?,
            RemoveOp::Dir(p) => stream.rmdir(p).await.map_err(map_ftp_err)?,
        }
    }
    Ok(())
}

/// Plan a depth-first (post-order) removal of a directory tree, listing each
/// directory via [`op_list_dir`]. The result removes children before parents.
async fn plan_ftp_removal<T>(
    stream: &mut ImplAsyncFtpStream<T>,
    root: &str,
) -> Result<Vec<RemoveOp>>
where
    T: TokioTlsStream + Send + 'static,
{
    let mut ops = Vec::new();
    let mut stack: Vec<(String, bool)> = vec![(root.to_string(), false)];
    while let Some((dir, expanded)) = stack.pop() {
        if expanded {
            ops.push(RemoveOp::Dir(dir));
            continue;
        }
        let entries = op_list_dir(stream, &RemotePath::new(&dir)).await?;
        stack.push((dir.clone(), true));
        for entry in entries {
            let child = format!("{dir}/{}", entry.name);
            if entry.kind == EntryKind::Directory {
                stack.push((child, false));
            } else {
                ops.push(RemoveOp::File(child));
            }
        }
    }
    Ok(ops)
}

/// Probe a single path's kind without changing server state where possible.
///
/// Prefers `MLST` (one round-trip, files and directories, no `CWD`); falls back
/// to `SIZE` (file) then a `CWD` probe (directory, working dir restored) for
/// servers without `MLST`. `Ok(None)` means the path does not exist.
async fn op_kind<T>(
    stream: &mut ImplAsyncFtpStream<T>,
    path: &RemotePath,
) -> Result<Option<EntryKind>>
where
    T: TokioTlsStream + Send + 'static,
{
    let p = path.as_str();
    match stream.mlst(Some(p)).await {
        Ok(line) => {
            if let Ok(file) = File::try_from(line.as_str()) {
                return Ok(Some(file_kind(&file)));
            }
        }
        Err(err) if is_not_found(&err) => return Ok(None),
        // MLST unsupported — fall through to SIZE/CWD probing.
        Err(_) => {}
    }
    match stream.size(p).await {
        Ok(_) => return Ok(Some(EntryKind::File)),
        Err(err) if is_not_found(&err) => {}
        Err(err) => return Err(map_ftp_err(err)),
    }
    let prev = stream.pwd().await.ok();
    let is_dir = stream.cwd(p).await.is_ok();
    if let Some(prev) = prev {
        let _ = stream.cwd(prev).await;
    }
    Ok(is_dir.then_some(EntryKind::Directory))
}

/// Map a parsed [`File`] to the shared [`EntryKind`].
fn file_kind(file: &File) -> EntryKind {
    if file.is_symlink() {
        EntryKind::Symlink
    } else if file.is_directory() {
        EntryKind::Directory
    } else if file.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

/// Reconstruct Unix permission bits (the low `0o777`) from a parsed listing's
/// owner/group/other rwx triads. FTP listings carry no file-type bits, so only
/// the permission bits are set; the entry kind is tracked separately.
fn perm_mode(file: &File) -> u32 {
    let triad = |who: PosixPexQuery| {
        (u32::from(file.can_read(who)) << 2)
            | (u32::from(file.can_write(who)) << 1)
            | u32::from(file.can_execute(who))
    };
    (triad(PosixPexQuery::Owner) << 6)
        | (triad(PosixPexQuery::Group) << 3)
        | triad(PosixPexQuery::Others)
}

/// Whether an FTP error is a "no such file/dir" (`550`) response.
fn is_not_found(err: &FtpError) -> bool {
    matches!(err, FtpError::UnexpectedResponse(resp) if resp.status.code() == 550)
}

/// Map a `suppaftp` error from an **established session** to [`NyxError`].
///
/// A dropped control connection (`ConnectionError`) becomes
/// [`NyxError::ConnectionLost`] so the service flips the session to "lost" and
/// offers a reconnect; server status replies map to ordinary op failures. Server
/// messages carry no credential (we never send one in a path).
pub(crate) fn map_ftp_err(err: FtpError) -> NyxError {
    match err {
        FtpError::ConnectionError(e) => NyxError::ConnectionLost(e.to_string()),
        FtpError::UnexpectedResponse(resp) => {
            let code = resp.status.code();
            let msg = resp.as_string().unwrap_or_default();
            match code {
                550 | 553 => NyxError::NotFound(msg),
                530 | 532 => NyxError::Auth,
                // Service-not-available / data-connection failures mid-session
                // mean the transport is effectively gone.
                421 | 425 | 426 => NyxError::ConnectionLost(msg),
                _ => NyxError::Io(format!("ftp error {code}: {msg}")),
            }
        }
        FtpError::SecureError(m) => NyxError::Connection(m),
        FtpError::BadResponse => NyxError::Io("invalid ftp server response".into()),
        FtpError::InvalidAddress(e) => NyxError::Connection(e.to_string()),
        FtpError::DataConnectionAlreadyOpen => {
            NyxError::Io("ftp data connection already open".into())
        }
    }
}

/// Map a `suppaftp` error from the **initial connect attempt** to [`NyxError`].
/// A failed attempt is [`NyxError::Connection`] (not `ConnectionLost`); a `530`
/// during login is [`NyxError::Auth`].
pub(crate) fn map_connect_err(err: FtpError) -> NyxError {
    match err {
        FtpError::ConnectionError(e) => NyxError::Connection(e.to_string()),
        other => map_ftp_err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_detects_550() {
        use suppaftp::types::Response;
        use suppaftp::Status;
        let err = FtpError::UnexpectedResponse(Response::new(
            Status::FileUnavailable,
            b"No such file".to_vec(),
        ));
        assert!(is_not_found(&err));
        assert!(matches!(map_ftp_err(err), NyxError::NotFound(_)));
    }

    #[test]
    fn dropped_control_connection_is_connection_lost() {
        let err = FtpError::ConnectionError(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "broken pipe",
        ));
        assert!(matches!(map_ftp_err(err), NyxError::ConnectionLost(_)));
    }

    #[test]
    fn connect_failure_is_a_plain_connection_error() {
        let err = FtpError::ConnectionError(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ));
        assert!(matches!(map_connect_err(err), NyxError::Connection(_)));
    }

    #[test]
    fn login_rejection_is_auth() {
        use suppaftp::types::Response;
        use suppaftp::Status;
        let err = FtpError::UnexpectedResponse(Response::new(
            Status::NotLoggedIn,
            b"Login failed".to_vec(),
        ));
        assert!(matches!(map_ftp_err(err), NyxError::Auth));
    }

    #[test]
    fn perm_mode_packs_rwx_triads() {
        // rwxr-x--- style: parse a POSIX LIST line and confirm the mode bits.
        let file = File::try_from("drwxr-x--- 2 user group 4096 Jan 1 00:00 sub").unwrap();
        assert_eq!(perm_mode(&file), 0o750);
    }
}
