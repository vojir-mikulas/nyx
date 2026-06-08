//! Transfer execution: the copy engine the dispatcher spawns.
//!
//! Pure mechanical move out of `lib.rs` (code review 2026-06-08, plan 05) - no
//! behaviour change. `use super::*` inherits the crate-root imports and the
//! dispatcher-side helpers these call back into.

use super::*;

/// The outcome of a spawned copy task, reported to the dispatcher.
pub(crate) enum TransferOutcome {
    /// The pre-flight gate found an existing destination and no resolved policy:
    /// the task wrote nothing and the dispatcher should park the item for a
    /// user decision. Not a terminal state.
    Collision {
        /// Size of the existing destination, if statted.
        existing_size: Option<u64>,
    },
    /// The copy finished and the remote writes were acknowledged. `message`
    /// carries a folder transfer's one-line skipped/failed tally, if any;
    /// `report` carries the per-entry detail behind it (folder transfers only).
    Completed {
        message: Option<String>,
        report: Option<TransferReport>,
    },
    /// The copy was cancelled mid-flight (the temp partial was cleaned up).
    Cancelled,
    /// The destination existed and the policy resolved to skip; nothing written.
    Skipped,
    /// The transport died mid-copy: the partial is **kept** for a resume. Carries
    /// the bytes-done watermark and the source fingerprint captured at start.
    /// Only file transfers produce this; the dispatcher flips the session to
    /// lost and parks the transfer in the queue's interrupted state.
    Interrupted {
        /// Bytes written so far (the resume offset).
        transferred: u64,
        /// The source fingerprint at start, for the resume's unchanged-guard.
        source_meta: Option<nyx_core::SourceMeta>,
    },
    /// The copy failed; the credential-free message is for the UI.
    Failed(String),
}

/// Map an `is_dir` flag to a [`TransferKind`].
pub(crate) fn kind_of(is_dir: bool) -> TransferKind {
    if is_dir {
        TransferKind::Dir
    } else {
        TransferKind::File
    }
}

/// Submit a transfer (file or directory) into the queue: guard on a live session,
/// build the spec, announce `TransferQueued`, then try to start it. Shared by the
/// `Download` and `Upload` commands (the only difference is direction).
#[allow(clippy::too_many_arguments)]
pub(crate) fn submit_transfer(
    queue: &mut TransferQueue,
    client: &Option<Arc<dyn RemoteClient>>,
    events: &FuturesSender<Event>,
    xfer_done: &TokioSender<(TransferId, u64, TransferOutcome)>,
    generation: u64,
    direction: TransferDirection,
    kind: TransferKind,
    remote: RemotePath,
    local: PathBuf,
) {
    if client.is_none() {
        not_connected(events);
        return;
    }
    // Defense in depth at the trust boundary: a download destination is built from
    // a server-supplied name; a `..` component means it tried to escape the chosen
    // folder. Legit picker/save-as/drop destinations never contain one.
    if direction == TransferDirection::Download
        && local
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        let _ = events.unbounded_send(Event::Error {
            message: "refusing download to a path containing '..'".into(),
        });
        return;
    }
    let spec = TransferSpec {
        direction,
        kind,
        remote: remote.clone(),
        local: local.clone(),
        on_collision: None,
        resume_from: 0,
        source_meta: None,
    };
    match queue.submit(spec) {
        Ok(id) => {
            let _ = events.unbounded_send(Event::TransferQueued {
                id,
                direction,
                kind,
                remote,
                local: local.display().to_string(),
            });
            try_start(queue, client, events, xfer_done, generation);
        }
        Err(_) => path_in_use(events, &remote),
    }
}

/// Promote and spawn as many queued transfers as the cap allows.
///
/// A missing session is a guard, not an error: queued transfers only exist while
/// connected (the senders check), and `Disconnect` drains the queue - so this is
/// just belt-and-braces against promoting a transfer with no session to run it.
pub(crate) fn try_start(
    queue: &mut TransferQueue,
    client: &Option<Arc<dyn RemoteClient>>,
    events: &FuturesSender<Event>,
    xfer_done: &TokioSender<(TransferId, u64, TransferOutcome)>,
    generation: u64,
) {
    let Some(client) = client else { return };
    while let Some(started) = queue.poll_start() {
        spawn_transfer(
            client.clone(),
            started,
            events.clone(),
            xfer_done.clone(),
            generation,
        );
    }
}

/// Spawn the copy task for a just-started transfer: stat the size, announce the
/// start, run the protocol copy into a sibling temp, rename it into place on
/// success (removing the temp on cancel/fail), and report the terminal outcome.
pub(crate) fn spawn_transfer(
    client: Arc<dyn RemoteClient>,
    started: Started,
    events: FuturesSender<Event>,
    xfer_done: TokioSender<(TransferId, u64, TransferOutcome)>,
    generation: u64,
) {
    let Started { id, spec, progress } = started;
    tokio::spawn(async move {
        // Pre-flight collision gate: stat the destination before writing a byte.
        // A reliability-first client must never blind-overwrite. A re-admitted
        // resume carries an `Overwrite` policy, so it skips the prompt here.
        if let Some(outcome) = collision_gate(&*client, &spec).await {
            let _ = xfer_done.send((id, generation, outcome));
            return;
        }

        let outcome = match spec.kind {
            TransferKind::File => copy_file(&*client, &spec, &progress, id, &events).await,
            TransferKind::Dir => copy_dir(&*client, &spec, &progress, id, &events).await,
        };
        let _ = xfer_done.send((id, generation, outcome));
    });
}

/// Copy a single file: capture the source fingerprint, decide the resume offset,
/// announce the start, run the protocol copy, and classify the outcome - a
/// transport death keeps the partial for a resume; any other error cleans it up.
pub(crate) async fn copy_file(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    progress: &nyx_core::TransferProgress,
    id: TransferId,
    events: &FuturesSender<Event>,
) -> TransferOutcome {
    // Fingerprint the source now. For a download the source is remote (only an
    // SFTP client reports it); for an upload it's the always-statable local file.
    // The fingerprint is carried into a resume to confirm the source is unchanged.
    let source_meta = capture_source_meta(client, spec).await;

    // Atomic destination: bytes are written to a sibling temp (`<name>.nyxpart`),
    // and the final path only ever appears via the atomic rename on success. A
    // cancelled/failed copy removes the temp; an interrupted one keeps it for the
    // resume - so the final path is never a half-written file masquerading as
    // complete, and a cancelled *overwrite* leaves the original intact.
    let tmp_local = local_part_path(&spec.local);
    let tmp_remote = remote_part_path(&spec.remote);

    // The effective offset is the **temp partial's actual on-disk size**, not the
    // watermark - the watermark can run ahead of durably-written bytes (an upload's
    // SFTP writes ack lazily), and resuming past the real EOF would leave a gap.
    // Only resume when the client can, the source is verifiably unchanged, and the
    // partial fits within it; otherwise restart from zero.
    let dest_size = if spec.resume_from > 0 {
        partial_temp_size(client, spec, &tmp_local, &tmp_remote).await
    } else {
        None
    };
    let offset = resume_offset(client.supports_resume(), spec, source_meta, dest_size);
    progress.seed(offset);

    // Stat the total up front so the dock can show a real %/total.
    let total = match spec.direction {
        TransferDirection::Download => client
            .remote_size(&spec.remote)
            .await
            .or_else(|| source_meta.map(|m| m.size)),
        TransferDirection::Upload => tokio::fs::metadata(&spec.local).await.ok().map(|m| m.len()),
    };
    let _ = events.unbounded_send(Event::TransferStarted { id, total });

    let result = match spec.direction {
        TransferDirection::Download => {
            client
                .download(&spec.remote, &tmp_local, progress, offset)
                .await
        }
        TransferDirection::Upload => {
            client
                .upload(&spec.local, &tmp_remote, progress, offset)
                .await
        }
    };
    match result {
        Ok(()) => {
            // Promote the temp into place. The rename is the overwrite for the
            // file case, so it fires only after the collision gate sanctioned it.
            let committed = match spec.direction {
                TransferDirection::Download => commit_local(&tmp_local, &spec.local).await,
                TransferDirection::Upload => {
                    commit_remote(client, &tmp_remote, &spec.remote, may_overwrite(spec)).await
                }
            };
            match committed {
                Ok(()) => TransferOutcome::Completed {
                    message: None,
                    report: None,
                },
                Err(err) => {
                    cleanup_file_temp(client, spec, &tmp_local, &tmp_remote).await;
                    TransferOutcome::Failed(err.to_string())
                }
            }
        }
        Err(NyxError::Cancelled) => {
            cleanup_file_temp(client, spec, &tmp_local, &tmp_remote).await;
            TransferOutcome::Cancelled
        }
        Err(err) => {
            // A transport loss is resumable: keep the temp partial, hand back the
            // watermark + fingerprint. A genuine error (disk full, permissions)
            // is terminal: clean up the temp. The probe disambiguates the two.
            if is_transport_lost(client, &spec.remote, &err).await {
                TransferOutcome::Interrupted {
                    transferred: progress.transferred(),
                    source_meta,
                }
            } else {
                cleanup_file_temp(client, spec, &tmp_local, &tmp_remote).await;
                TransferOutcome::Failed(err.to_string())
            }
        }
    }
}

/// Capture the source file's `(size, mtime)` fingerprint at the start of a copy,
/// used to guard a later resume. The source is the **remote** file for a download
/// (reported only by resume-capable clients) and the **local** file for an upload.
pub(crate) async fn capture_source_meta(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
) -> Option<nyx_core::SourceMeta> {
    match spec.direction {
        TransferDirection::Download => client.remote_meta(&spec.remote).await,
        TransferDirection::Upload => {
            let meta = tokio::fs::metadata(&spec.local).await.ok()?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            Some(nyx_core::SourceMeta {
                size: meta.len(),
                mtime,
            })
        }
    }
}

/// The current size of a copy's **temp partial** - the local temp for a
/// download, the remote temp for an upload. This is the source of truth for the
/// resume offset (the watermark can run ahead of durably-written bytes), and the
/// partial lives at the temp path, not the final one.
pub(crate) async fn partial_temp_size(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    tmp_local: &Path,
    tmp_remote: &RemotePath,
) -> Option<u64> {
    match spec.direction {
        TransferDirection::Download => tokio::fs::metadata(tmp_local).await.ok().map(|m| m.len()),
        TransferDirection::Upload => client.remote_size(tmp_remote).await,
    }
}

/// The byte offset a file copy should actually start from: the destination's
/// real `dest_size`, but only when this is a resume (`resume_from > 0`), the
/// client supports it, the source is verifiably unchanged (same size + mtime),
/// and the partial fits within the source. On any doubt - a changed source, a
/// missing mtime, an unverifiable fingerprint, an oversized partial - restart
/// from `0` rather than splice bytes blind. Silent corruption is worse than a
/// re-transfer.
pub(crate) fn resume_offset(
    supports_resume: bool,
    spec: &TransferSpec,
    current: Option<nyx_core::SourceMeta>,
    dest_size: Option<u64>,
) -> u64 {
    if spec.resume_from == 0 || !supports_resume {
        return 0;
    }
    match (spec.source_meta, current, dest_size) {
        (Some(orig), Some(cur), Some(dest))
            if orig == cur && orig.mtime.is_some() && dest <= cur.size =>
        {
            dest
        }
        _ => 0,
    }
}

/// Whether a failed file copy was a transport loss (→ resumable) rather than a
/// genuine error (→ terminal). An error already typed [`NyxError::ConnectionLost`]
/// is decisive; otherwise probe the session with a cheap stat - if that itself
/// reports the connection gone, the copy died with it.
pub(crate) async fn is_transport_lost(
    client: &dyn RemoteClient,
    remote: &RemotePath,
    err: &NyxError,
) -> bool {
    if matches!(err, NyxError::ConnectionLost(_)) {
        return true;
    }
    // The mid-copy byte loop surfaces a remote transport death as a generic I/O
    // error, so confirm with a probe: a live session answers (Ok), a dead one
    // maps to ConnectionLost.
    matches!(
        client.exists(remote).await,
        Err(NyxError::ConnectionLost(_))
    )
}

/// Copy a whole directory tree as one aggregate transfer: enumerate it (so the
/// dock shows a real total), create the destination root, then walk the items
/// parent-before-child, reusing the single-file `download`/`upload` primitives.
///
/// Per the settled decisions: collisions merge (each file overwrites in place),
/// a failed/unreadable file is **skipped and tallied** (one bad file never aborts
/// the folder), symlinks are skipped during the walk, and empty directories are
/// created. Each file rides the atomic temp-then-rename, so the tree never holds a
/// half-written file - only a subset of complete ones. Cancellation is checked
/// between items; a cancelled folder keeps the complete files it copied (we never
/// delete a merge destination), pruning only a root we created and left empty.
pub(crate) async fn copy_dir(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    progress: &nyx_core::TransferProgress,
    id: TransferId,
    events: &FuturesSender<Event>,
) -> TransferOutcome {
    // Enumerate before announcing, so `total` is the real byte sum.
    let walk = match enumerate_dir(client, spec).await {
        Ok(walk) => walk,
        Err(err) => return TransferOutcome::Failed(err.to_string()),
    };
    let _ = events.unbounded_send(Event::TransferStarted {
        id,
        total: Some(walk.total_bytes),
    });

    // Create the destination root, remembering whether it pre-existed: a root we
    // created (no merge) is safe to prune back if the transfer is cancelled before
    // any file lands; a pre-existing merge target is the user's data - never touch.
    let created_root = match make_root(client, spec).await {
        Ok(created) => created,
        Err(err) => return TransferOutcome::Failed(err.to_string()),
    };

    let mut failed = 0u64;
    let mut issues: Vec<EntryIssue> = Vec::new();
    for item in &walk.items {
        if progress.is_cancelled() {
            prune_created_root(client, spec, created_root).await;
            return TransferOutcome::Cancelled;
        }
        match copy_walk_item(client, spec, item, progress).await {
            Ok(()) => {}
            Err(NyxError::Cancelled) => {
                prune_created_root(client, spec, created_root).await;
                return TransferOutcome::Cancelled;
            }
            Err(err) => {
                debug!(error = %err, rel = ?item.rel, "skipping unreadable entry in folder transfer");
                failed += 1;
                push_capped(
                    &mut issues,
                    EntryIssue::failed(item.rel.join("/"), err.to_string()),
                );
            }
        }
    }

    let skipped = walk.skips.len() as u64;
    for skip in walk.skips {
        push_capped(&mut issues, skip);
    }

    let report = TransferReport {
        failed,
        skipped,
        issues,
    };
    let message = report.summary();
    let report = report.has_issues().then_some(report);
    TransferOutcome::Completed { message, report }
}

/// Append `issue` to the retained list only while it is under the cap - full
/// counts stay exact, but a folder with thousands of bad entries never ships a
/// thousands-long report. The dropped tail is surfaced via
/// [`TransferReport::truncated`].
pub(crate) fn push_capped(issues: &mut Vec<EntryIssue>, issue: EntryIssue) {
    const ISSUE_CAP: usize = 100;
    if issues.len() < ISSUE_CAP {
        issues.push(issue);
    }
}

/// Enumerate a directory transfer's work items + totals - a remote walk for a
/// download, a local-filesystem walk for an upload.
pub(crate) async fn enumerate_dir(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
) -> Result<DirWalk, NyxError> {
    match spec.direction {
        TransferDirection::Download => client.walk_dir(&spec.remote).await,
        TransferDirection::Upload => local_walk(&spec.local).await,
    }
}

/// Create the destination root of a directory transfer (idempotent: an existing
/// root is fine - that is the merge case). Returns `true` when the root did **not**
/// pre-exist (we created it), so a later cancel can safely prune it.
pub(crate) async fn make_root(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
) -> Result<bool, NyxError> {
    match spec.direction {
        TransferDirection::Download => {
            let existed = tokio::fs::try_exists(&spec.local).await.unwrap_or(false);
            tokio::fs::create_dir_all(&spec.local)
                .await
                .map_err(|e| NyxError::Io(e.to_string()))?;
            Ok(!existed)
        }
        TransferDirection::Upload => {
            let existed = client.exists(&spec.remote).await.unwrap_or(false);
            ensure_remote_dir(client, &spec.remote).await?;
            Ok(!existed)
        }
    }
}

/// Copy one walk item to its mirrored destination under the transfer's root. File
/// items go through the atomic temp-then-rename so the tree never holds a
/// half-written file - only a subset of complete ones. A folder transfer is a
/// sanctioned merge, so an item may overwrite an existing file in the tree.
pub(crate) async fn copy_walk_item(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    item: &WalkItem,
    progress: &nyx_core::TransferProgress,
) -> Result<(), NyxError> {
    // Defense in depth: the walker already rejects unsafe names, but never let a
    // server-derived component reach a local `push` without re-checking - a `..`
    // or absolute segment must not escape the download destination.
    if spec.direction == TransferDirection::Download
        && !item.rel.iter().all(|seg| is_safe_local_segment(seg))
    {
        return Err(NyxError::Other(format!(
            "refusing unsafe destination path for {}",
            item.rel.join("/")
        )));
    }
    let remote = join_remote(&spec.remote, &item.rel);
    let local = join_local(&spec.local, &item.rel);
    match (spec.direction, item.is_dir) {
        (TransferDirection::Download, true) => tokio::fs::create_dir_all(&local)
            .await
            .map_err(|e| NyxError::Io(e.to_string())),
        // Directory transfers don't resume per-item yet - always copy from 0.
        (TransferDirection::Download, false) => {
            atomic_download_file(client, &remote, &local, progress).await
        }
        (TransferDirection::Upload, true) => ensure_remote_dir(client, &remote).await,
        (TransferDirection::Upload, false) => {
            atomic_upload_file(client, &local, &remote, progress).await
        }
    }
}

/// Download one file inside a folder transfer atomically: write to a sibling temp,
/// rename into place on success, and remove the temp on any error so the tree only
/// ever holds complete files.
pub(crate) async fn atomic_download_file(
    client: &dyn RemoteClient,
    remote: &RemotePath,
    local: &Path,
    progress: &nyx_core::TransferProgress,
) -> Result<(), NyxError> {
    let tmp = local_part_path(local);
    let result = match client.download(remote, &tmp, progress, 0).await {
        Ok(()) => commit_local(&tmp, local).await,
        Err(err) => Err(err),
    };
    if result.is_err() {
        remove_local_temp(&tmp).await;
    }
    result
}

/// Upload one file inside a folder transfer atomically (the upload mirror of
/// [`atomic_download_file`]); the merge permits overwriting an existing file.
pub(crate) async fn atomic_upload_file(
    client: &dyn RemoteClient,
    local: &Path,
    remote: &RemotePath,
    progress: &nyx_core::TransferProgress,
) -> Result<(), NyxError> {
    let tmp = remote_part_path(remote);
    let result = match client.upload(local, &tmp, progress, 0).await {
        Ok(()) => commit_remote(client, &tmp, remote, true).await,
        Err(err) => Err(err),
    };
    if result.is_err() {
        remove_remote_temp(client, &tmp).await;
    }
    result
}

/// `mkdir` that tolerates an already-existing directory (the merge case).
pub(crate) async fn ensure_remote_dir(
    client: &dyn RemoteClient,
    path: &RemotePath,
) -> Result<(), NyxError> {
    match client.mkdir(path).await {
        Ok(()) => Ok(()),
        Err(err) => {
            if client.exists(path).await.unwrap_or(false) {
                Ok(())
            } else {
                Err(err)
            }
        }
    }
}

/// Join walk-item components onto a remote root.
pub(crate) fn join_remote(root: &RemotePath, rel: &[String]) -> RemotePath {
    rel.iter().fold(root.clone(), |p, seg| p.join(seg))
}

/// Join walk-item components onto a local root.
pub(crate) fn join_local(root: &std::path::Path, rel: &[String]) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in rel {
        p.push(seg);
    }
    p
}

/// Walk a local directory tree on the service thread, mirroring the remote
/// [`RemoteClient::walk_dir`]: pre-order, symlinks (and non-utf8 / special
/// entries) skipped and tallied, file sizes summed. No async recursion - an
/// explicit stack of directories to visit.
pub(crate) async fn local_walk(root: &std::path::Path) -> Result<DirWalk, NyxError> {
    let mut walk = DirWalk::default();
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(root.to_path_buf(), Vec::new())];
    while let Some((dir, rel)) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .map_err(|e| NyxError::Io(e.to_string()))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| NyxError::Io(e.to_string()))?
        {
            let raw_name = entry.file_name();
            let Some(name) = raw_name.to_str() else {
                // Non-UTF-8 names are not representable remotely - skip, but still
                // surface a (lossy) path so the report names the offending entry.
                let mut shown = rel.clone();
                shown.push(raw_name.to_string_lossy().into_owned());
                walk.skips
                    .push(EntryIssue::skipped(shown.join("/"), "non-UTF-8 name"));
                continue;
            };
            let mut child_rel = rel.clone();
            child_rel.push(name.to_string());
            // `symlink_metadata` is lstat-style, so a link is reported as a link.
            let meta = match tokio::fs::symlink_metadata(entry.path()).await {
                Ok(meta) => meta,
                Err(err) => {
                    walk.skips.push(EntryIssue::skipped(
                        child_rel.join("/"),
                        format!("unreadable: {err}"),
                    ));
                    continue;
                }
            };
            let ft = meta.file_type();
            if ft.is_symlink() {
                walk.skips
                    .push(EntryIssue::skipped(child_rel.join("/"), "symlink skipped"));
            } else if !ft.is_dir() && !ft.is_file() {
                walk.skips.push(EntryIssue::skipped(
                    child_rel.join("/"),
                    "special file skipped",
                ));
            } else if ft.is_dir() {
                walk.items.push(WalkItem {
                    rel: child_rel.clone(),
                    is_dir: true,
                    size: 0,
                });
                stack.push((entry.path(), child_rel));
            } else {
                walk.total_bytes += meta.len();
                walk.items.push(WalkItem {
                    rel: child_rel,
                    is_dir: false,
                    size: meta.len(),
                });
            }
        }
    }
    Ok(walk)
}

/// The transfer pre-flight gate. Stats the destination (remote for an upload,
/// local for a download) **before** any bytes move.
///
/// Returns `Some(outcome)` when the task must stop without writing:
/// - the destination exists and the policy is unresolved (`None`) → `Collision`
///   (the dispatcher parks the item and prompts);
/// - it exists and the policy is `Skip`/`Cancel` → the matching terminal.
///
/// Returns `None` to proceed with the copy - either no collision, or the policy
/// is `Overwrite`.
///
/// A stat **error** (permission denied, transient I/O) is treated as *possibly
/// present*, not *absent* - so an unreadable destination prompts the user (or
/// honors a `Skip`) instead of being silently overwritten. Only a definite
/// `Ok(false)` skips the gate. A genuine `Overwrite` still proceeds regardless,
/// since the user already sanctioned replacing whatever is there.
pub(crate) async fn collision_gate(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
) -> Option<TransferOutcome> {
    let exists = match spec.direction {
        TransferDirection::Download => treat_as_present(tokio::fs::try_exists(&spec.local).await),
        TransferDirection::Upload => treat_as_present(client.exists(&spec.remote).await),
    };
    if !exists {
        return None;
    }
    match spec.on_collision {
        None => {
            // A directory merge has no single "existing size"; only stat a file.
            let existing_size = match (spec.kind, spec.direction) {
                (TransferKind::Dir, _) => None,
                (TransferKind::File, TransferDirection::Download) => {
                    tokio::fs::metadata(&spec.local).await.ok().map(|m| m.len())
                }
                (TransferKind::File, TransferDirection::Upload) => {
                    client.remote_size(&spec.remote).await
                }
            };
            Some(TransferOutcome::Collision { existing_size })
        }
        Some(CollisionChoice::Skip) => Some(TransferOutcome::Skipped),
        Some(CollisionChoice::Cancel) => Some(TransferOutcome::Cancelled),
        Some(CollisionChoice::Overwrite) => None,
    }
}

/// How the collision gate reads a destination-existence probe. Only a definite
/// `Ok(false)` (the destination is absent) skips the gate; an error is treated as
/// *possibly present* so an unreadable destination is never silently overwritten -
/// it prompts (or honors `Skip`) instead. Pinned by a test so a future "simplify
/// to `unwrap_or(false)`" can't quietly reintroduce the blind-overwrite footgun.
pub(crate) fn treat_as_present<E>(probe: std::result::Result<bool, E>) -> bool {
    probe.unwrap_or(true)
}

/// The suffix marking a Nyx partial-transfer temp file. Deterministic and
/// recognizable so a crash-left temp can be identified (and a resume can find the
/// partial), never mistaken for user data and auto-deleted blindly.
pub(crate) const PART_SUFFIX: &str = ".nyxpart";

/// Sibling temp path for an atomic local write: `<name>.nyxpart` in the same
/// directory as `local`, so the rename into place is same-volume (atomic).
pub(crate) fn local_part_path(local: &Path) -> PathBuf {
    let mut name = local
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(PART_SUFFIX);
    local.with_file_name(name)
}

/// Sibling temp path for an atomic remote write - the remote mirror of
/// [`local_part_path`].
pub(crate) fn remote_part_path(remote: &RemotePath) -> RemotePath {
    match (remote.parent(), remote.file_name()) {
        (Some(parent), Some(name)) => parent.join(&format!("{name}{PART_SUFFIX}")),
        // The root has no name and is never a transfer target; leave it as-is.
        _ => remote.clone(),
    }
}

/// Whether a copy is allowed to replace an existing final destination: only when
/// the collision gate resolved to `Overwrite` (or a resume re-admitted with it).
pub(crate) fn may_overwrite(spec: &TransferSpec) -> bool {
    spec.on_collision == Some(CollisionChoice::Overwrite)
}

/// Promote a local temp into its final path. The rename atomically replaces any
/// existing file (same volume), so a cancelled overwrite leaves the original.
pub(crate) async fn commit_local(tmp: &Path, final_path: &Path) -> Result<(), NyxError> {
    tokio::fs::rename(tmp, final_path)
        .await
        .map_err(|e| NyxError::Io(e.to_string()))
}

/// Promote a remote temp into its final path. SFTP/FTP `rename` is the atomic
/// case, but some servers refuse to rename onto an existing path - so when (and
/// only when) overwriting was sanctioned and the final exists, remove it and
/// retry. Never a blind delete of a destination the user didn't choose to replace.
pub(crate) async fn commit_remote(
    client: &dyn RemoteClient,
    tmp: &RemotePath,
    final_path: &RemotePath,
    may_overwrite: bool,
) -> Result<(), NyxError> {
    match client.rename(tmp, final_path).await {
        Ok(()) => Ok(()),
        Err(err) => {
            if may_overwrite && client.exists(final_path).await.unwrap_or(false) {
                client.remove(final_path).await?;
                client.rename(tmp, final_path).await
            } else {
                Err(err)
            }
        }
    }
}

/// Best-effort removal of a cancelled/failed file transfer's temp partial: the
/// local temp for a download, the remote temp for an upload. The final path was
/// never touched (only a successful rename creates it), so there is nothing else
/// to clean. Errors are logged at `debug` - the terminal `TransferDone` tells the
/// real story.
pub(crate) async fn cleanup_file_temp(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    tmp_local: &Path,
    tmp_remote: &RemotePath,
) {
    match spec.direction {
        TransferDirection::Download => remove_local_temp(tmp_local).await,
        TransferDirection::Upload => remove_remote_temp(client, tmp_remote).await,
    }
}

/// Remove a local temp, ignoring a missing file (a cancel before any byte landed).
pub(crate) async fn remove_local_temp(tmp: &Path) {
    match tokio::fs::remove_file(tmp).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => debug!(error = %err, "could not remove partial download temp"),
    }
}

/// Remove a remote temp (best-effort; a missing temp is fine).
pub(crate) async fn remove_remote_temp(client: &dyn RemoteClient, tmp: &RemotePath) {
    if let Err(err) = client.remove(tmp).await {
        debug!(error = %err, "could not remove partial upload temp");
    }
}

/// On a cancelled folder transfer, remove the destination root **only if we
/// created it** (it didn't pre-exist, so it can't be a merge target holding the
/// user's data) **and it still holds no files** (every completed file is kept).
/// Conservative: a read error or any file present leaves the whole tree in place.
pub(crate) async fn prune_created_root(
    client: &dyn RemoteClient,
    spec: &TransferSpec,
    created_root: bool,
) {
    if !created_root {
        return;
    }
    let fileless = match spec.direction {
        TransferDirection::Download => local_tree_fileless(&spec.local).await,
        TransferDirection::Upload => remote_tree_fileless(client, &spec.remote).await,
    };
    if fileless != Some(true) {
        return;
    }
    match spec.direction {
        TransferDirection::Download => {
            if let Err(err) = tokio::fs::remove_dir_all(&spec.local).await {
                debug!(error = %err, "could not prune empty created download root");
            }
        }
        TransferDirection::Upload => {
            if let Err(err) = client.remove(&spec.remote).await {
                debug!(error = %err, "could not prune empty created upload root");
            }
        }
    }
}

/// Whether a local tree holds only (empty-of-files) directories - `None` on any
/// read error, so the caller leaves the tree alone when it can't be sure.
pub(crate) async fn local_tree_fileless(root: &Path) -> Option<bool> {
    let walk = local_walk(root).await.ok()?;
    Some(walk.items.iter().all(|i| i.is_dir) && walk.skips.is_empty())
}

/// The remote mirror of [`local_tree_fileless`].
pub(crate) async fn remote_tree_fileless(
    client: &dyn RemoteClient,
    root: &RemotePath,
) -> Option<bool> {
    let walk = client.walk_dir(root).await.ok()?;
    Some(walk.items.iter().all(|i| i.is_dir) && walk.skips.is_empty())
}
