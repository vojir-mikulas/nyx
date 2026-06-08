//! Downloads, uploads, in-app and OS drag-drop, collision resolution, and transfer-report actions.

use super::*;

impl AppState {
    /// Download the current selection. A single file opens a save-as dialog;
    /// anything else - several entries, or a single folder - opens a folder
    /// picker and issues one `Download` per top-level entry (folders recurse).
    pub fn download_selection(&mut self, cx: &mut Context<Self>) {
        self.close_file_menu();
        // (remote path, display name, is_dir) for each selected entry.
        let mut entries: Vec<(RemotePath, String, bool)> = Vec::new();
        let mut skipped_unsafe = false;
        for name in &self.selected {
            let Some(row) = self
                .listing
                .iter()
                .find(|row| row.entry.name.as_str() == name.as_ref())
            else {
                continue;
            };
            // The name becomes a local path segment (`folder.join(name)`); a hostile
            // server listing a `..`/separator name must not escape the chosen folder.
            if !is_safe_local_segment(name) {
                skipped_unsafe = true;
                continue;
            }
            entries.push((self.cwd.join(name), name.to_string(), row.entry.is_dir()));
        }
        if skipped_unsafe {
            self.push_toast(
                "Skipped entries with unsafe names from the server",
                ToastVariant::Error,
                cx,
            );
        }
        if entries.is_empty() {
            return;
        }

        // A lone file gets the familiar save-as dialog; a lone folder or a batch
        // picks a destination folder to drop the items into.
        if let [(remote, name, false)] = entries.as_slice() {
            self.download_remote_file(remote.clone(), name.clone(), cx);
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Download to".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(folder))) = receiver.await else {
                return;
            };
            let Some(folder) = folder.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                let mut ok = true;
                for (remote, name, is_dir) in entries {
                    let local = folder.join(&name);
                    if !this.service.send(Command::Download {
                        remote,
                        local,
                        is_dir,
                    }) {
                        ok = false;
                    }
                }
                if !ok {
                    this.push_toast("Backend unavailable", ToastVariant::Error, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Download a single remote file: open a save-as dialog (defaulting to the OS
    /// Downloads folder + `name`), then issue the `Download`. Shared by the
    /// single-file selection path and symlink-to-file resolution.
    pub(super) fn download_remote_file(
        &mut self,
        remote: RemotePath,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let dir = default_download_dir();
        let receiver = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(local))) = receiver.await {
                this.update(cx, |this, cx| {
                    if !this.service.send(Command::Download {
                        remote,
                        local,
                        is_dir: false,
                    }) {
                        this.push_toast("Backend unavailable", ToastVariant::Error, cx);
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Move `names` into the child directory `dir` via one server-side `Rename`
    /// per item. Skips the directory itself, any folder dropped onto its own
    /// subtree, and symlinks (mirroring the drag-out rules). The listing
    /// refreshes per item as each `Rename` completes ([`Event::FileOpDone`]).
    pub fn move_into(
        &mut self,
        dir: &SharedString,
        names: Vec<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let dest = self.cwd.join(dir);
        tracing::info!(?dir, ?names, "move_into");
        let mut ok = true;
        for name in &names {
            let Some(row) = self
                .listing
                .iter()
                .find(|row| row.entry.name.as_str() == name.as_ref())
            else {
                tracing::info!(?name, "move_into: row not found in listing, skipping");
                continue;
            };
            if matches!(row.entry.kind, EntryKind::Symlink) {
                tracing::info!(?name, "move_into: symlink, skipping");
                continue;
            }
            let from = self.cwd.join(name);
            // `dest == from` (drop onto itself) or `dest` inside `from` (a folder
            // onto its own descendant) are both no-ops.
            if dest.is_within(&from) {
                tracing::info!(?from, ?dest, "move_into: dest within source, skipping");
                continue;
            }
            let to = dest.join(name);
            tracing::info!(?from, ?to, "move_into: sending Rename");
            if !self.service.send(Command::Rename { from, to }) {
                ok = false;
            }
        }
        if !ok {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Promote an in-app drag to a native OS drag-out of `names` to
    /// Finder/desktop; a folder drops as a recursive download. Each item streams
    /// through the download queue via the promise callback in [`crate::drag`].
    /// Returns whether the native session actually started - `false` when nothing
    /// was draggable (all symlinks/missing) or the platform refused - so the
    /// caller can keep the in-app drag alive on failure.
    pub fn start_native_drag(
        &mut self,
        names: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut files = Vec::new();
        let mut remotes = HashMap::new();
        for n in &names {
            let Some(row) = self
                .listing
                .iter()
                .find(|row| row.entry.name.as_str() == n.as_ref())
            else {
                continue;
            };
            // Symlinks aren't promised out (their target kind is unresolved here).
            if matches!(row.entry.kind, EntryKind::Symlink) {
                continue;
            }
            // The name becomes the dropped file's local name; a `..`/separator name
            // from the server must not escape the OS-chosen drop directory.
            if !is_safe_local_segment(n) {
                continue;
            }
            let is_dir = row.entry.is_dir();
            files.push(DragFile {
                name: n.to_string(),
                size: (!is_dir).then_some(row.entry.size),
                is_dir,
            });
            remotes.insert(n.to_string(), (self.cwd.join(n), is_dir));
        }
        if files.is_empty() {
            return false;
        }
        let fetch = Arc::new(ServiceDragFetch::new(
            self.service.commands(),
            self.drag_downloads.clone(),
            remotes,
        ));
        // Channels bridge the OS drag callbacks (fired on the UI thread by AppKit,
        // with no GPUI context) into `cx`-bearing tasks: a oneshot for the end
        // (act on a drop back inside the window) and a stream for moves (highlight
        // the folder under the cursor while the drag is inside).
        let (end_tx, end_rx) = oneshot::channel::<nyx_drag::DragEnd>();
        let (move_tx, mut move_rx) = futures::channel::mpsc::unbounded::<Option<(f32, f32)>>();
        let handlers = nyx_drag::DragHandlers {
            on_end: Some(Box::new(move |end| {
                let _ = end_tx.send(end);
            })),
            on_move: Some(Box::new(move |p| {
                let _ = move_tx.unbounded_send(p);
            })),
        };
        match nyx_drag::start_file_drag(window, files, fetch, None, handlers) {
            Ok(_) => {
                cx.spawn(async move |this, cx| {
                    while let Some(p) = move_rx.next().await {
                        if this
                            .update(cx, |this, cx| this.update_drag_return_highlight(p, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
                cx.spawn(async move |this, cx| {
                    if let Ok(end) = end_rx.await {
                        this.update(cx, |this, cx| {
                            this.drag_return_folder = None;
                            this.on_drag_returned(names, end, cx);
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
                true
            }
            Err(err) => {
                self.push_toast(
                    format!("Couldn't start drag: {err}"),
                    ToastVariant::Error,
                    cx,
                );
                false
            }
        }
    }

    /// The in-app drag's pointer left the window: hand off to the native OS drag
    /// of `names`. Promotion is **one-way** - once the native session starts we
    /// end the in-app drag so macOS owns the gesture (it can only finish as a
    /// drop-to-local). On failure the in-app drag stays live.
    pub fn handoff_drag_out(
        &mut self,
        names: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.start_native_drag(names, window, cx) {
            cx.stop_active_drag(window);
        }
    }

    /// Clear the recorded folder-row rects (start of a file-table render pass).
    /// Paint then repopulates them via [`AppState::drop_row_bounds_sink`].
    pub fn clear_drop_row_bounds(&self) {
        self.drop_row_bounds.borrow_mut().clear();
    }

    /// A handle to the folder-row-bounds sink, for the table's paint callback.
    pub fn drop_row_bounds_sink(&self) -> DropRowBounds {
        self.drop_row_bounds.clone()
    }

    /// The returning OS drag moved: highlight the folder row under the cursor (or
    /// clear the highlight when it's over nothing droppable). Only notifies on a
    /// change, so the frequent move callback doesn't thrash rendering.
    pub(super) fn update_drag_return_highlight(
        &mut self,
        p: Option<(f32, f32)>,
        cx: &mut Context<Self>,
    ) {
        let folder = p.and_then(|(x, y)| {
            let point = point(px(x), px(y));
            self.drop_row_bounds
                .borrow()
                .iter()
                .find(|(_, bounds)| bounds.contains(&point))
                .map(|(name, _)| name.clone())
        });
        if folder != self.drag_return_folder {
            self.drag_return_folder = folder;
            cx.notify();
        }
    }

    /// The OS drag-out ended. If no external target accepted it and it was
    /// released over one of our folder rows, treat it as an in-app move instead
    /// of a drop-to-local - the Phase 3 re-entry case (the cursor can't be
    /// demoted back to an in-app drag, but the *drop* still becomes a move).
    pub(super) fn on_drag_returned(
        &mut self,
        names: Vec<SharedString>,
        end: nyx_drag::DragEnd,
        cx: &mut Context<Self>,
    ) {
        if end.accepted {
            return; // an external target took the files (a real drop-to-local)
        }
        let Some((x, y)) = end.local else {
            return;
        };
        let point = point(px(x), px(y));
        let folder = self
            .drop_row_bounds
            .borrow()
            .iter()
            .find(|(_, bounds)| bounds.contains(&point))
            .map(|(name, _)| name.clone());
        if let Some(folder) = folder {
            self.move_into(&folder, names, cx);
        }
    }

    /// Upload one or more chosen local files into the current directory.
    pub fn upload(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Upload".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.upload_paths(paths, None, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Upload already-known local paths (from a drag-and-drop). `subdir`, when
    /// set, is a directory *in the current folder* the items were dropped onto;
    /// otherwise they land in the current folder. A dropped folder uploads
    /// recursively.
    pub fn upload_paths(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        subdir: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        // Only meaningful while browsing a connection.
        if self.view != View::Browse {
            return;
        }
        let mut ok = true;
        let mut sent = 0;
        for local in paths {
            let is_dir = local.is_dir();
            let Some(name) = local.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let remote = match subdir.as_ref() {
                Some(dir) => self.cwd.join(dir).join(name),
                None => self.cwd.join(name),
            };
            if self.service.send(Command::Upload {
                local,
                remote,
                is_dir,
            }) {
                sent += 1;
            } else {
                ok = false;
            }
        }
        if !ok {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        } else if sent > 0 {
            let into = subdir
                .as_deref()
                .map(|d| format!(" to {d}/"))
                .unwrap_or_default();
            let label = if sent == 1 {
                format!("Uploading 1 file{into}")
            } else {
                format!("Uploading {sent} files{into}")
            };
            self.push_toast(label, ToastVariant::Info, cx);
        }
    }

    /// Toggle the collision modal's "Apply to all" switch.
    pub fn set_collision_apply_all(&mut self, on: bool) {
        self.collision_apply_all = on;
    }

    /// Resolve the front pending collision with `choice`. With "apply to all" on,
    /// the service stamps every other pending/queued transfer, so we clear the
    /// whole local queue; otherwise we advance to the next parked transfer.
    pub fn resolve_collision(&mut self, choice: CollisionChoice, cx: &mut Context<Self>) {
        if self.pending_collisions.is_empty() {
            return;
        }
        let id = self.pending_collisions[0].id;
        let apply_to_all = self.collision_apply_all;
        if !self.service.send(Command::ResolveCollision {
            id,
            choice,
            apply_to_all,
        }) {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
            return;
        }
        if apply_to_all {
            self.pending_collisions.clear();
            self.collision_apply_all = false;
        } else {
            self.pending_collisions.remove(0);
            if self.pending_collisions.is_empty() {
                self.collision_apply_all = false;
            }
        }
    }

    /// Cancel a queued or running transfer (the dock's `x` button). The row
    /// updates reactively when the matching [`Event::TransferDone`] arrives.
    pub fn cancel_transfer(&mut self, id: TransferId) {
        self.service.send(Command::CancelTransfer { id });
    }

    /// Toggle the per-entry report disclosure on a completed-with-issues folder
    /// row (the dock's chevron / row click).
    pub fn toggle_transfer_report(&mut self, id: TransferId) {
        if let Some(vm) = self.transfers.iter_mut().find(|t| t.transfer.id == id) {
            vm.report_expanded = !vm.report_expanded;
        }
    }

    /// Copy a folder transfer's per-entry report to the clipboard as plain text;
    /// the whole point of surfacing the detail is that it can be pasted into a
    /// bug report. The text mirrors the (capped) retained list and notes any
    /// truncated tail so the paste is never silently partial.
    pub fn copy_transfer_report(&mut self, id: TransferId, cx: &mut Context<Self>) {
        let Some(vm) = self.transfers.iter().find(|t| t.transfer.id == id) else {
            return;
        };
        let Some(report) = vm.report.as_ref() else {
            return;
        };
        let mut out = String::new();
        out.push_str(&format!(
            "Folder transfer: {}\n",
            vm.transfer.remote_path.as_str()
        ));
        if let Some(summary) = report.summary() {
            out.push_str(&summary);
            out.push('\n');
        }
        let mut push_group = |kind: EntryOutcomeKind, label: &str| {
            let group: Vec<_> = report.issues.iter().filter(|i| i.kind == kind).collect();
            if group.is_empty() {
                return;
            }
            out.push_str(&format!("\n{label}:\n"));
            for issue in group {
                out.push_str(&format!("  {} - {}\n", issue.rel, issue.reason));
            }
        };
        push_group(EntryOutcomeKind::Failed, "Failed");
        push_group(EntryOutcomeKind::Skipped, "Skipped");
        let truncated = report.truncated();
        if truncated > 0 {
            out.push_str(&format!("…and {truncated} more\n"));
        }
        cx.write_to_clipboard(ClipboardItem::new_string(out));
        self.push_toast("Report copied", ToastVariant::Info, cx);
    }

    /// Re-issue a failed transfer (the dock's retry button). Resends the original
    /// `Upload`/`Download` command and drops the stale failed row - the retry
    /// re-enters the queue as a fresh transfer (its own `TransferQueued` event).
    pub fn retry_transfer(&mut self, id: TransferId, cx: &mut Context<Self>) {
        let Some(vm) = self.transfers.iter().find(|t| t.transfer.id == id) else {
            return;
        };
        if vm.transfer.status != TransferStatus::Failed {
            return;
        }
        let remote = vm.transfer.remote_path.clone();
        let local = std::path::PathBuf::from(vm.transfer.local_path.clone());
        let is_dir = vm.transfer.kind == TransferKind::Dir;
        let command = match vm.transfer.direction {
            TransferDirection::Upload => Command::Upload {
                local,
                remote,
                is_dir,
            },
            TransferDirection::Download => Command::Download {
                remote,
                local,
                is_dir,
            },
        };
        if self.service.send(command) {
            self.transfers.retain(|t| t.transfer.id != id);
        } else {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// `(active count, total speed bytes/sec)` across running transfers.
    pub fn active_speed(&self) -> (usize, u64) {
        let running: Vec<&TransferVm> = self
            .transfers
            .iter()
            .filter(|t| t.transfer.status == TransferStatus::Running)
            .collect();
        let speed = running.iter().filter_map(|t| t.speed_bps).sum();
        (running.len(), speed)
    }
}
