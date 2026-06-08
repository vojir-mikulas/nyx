//! The service-event reducer.

use super::*;

impl AppState {
    /// Apply a backend [`Event`] to the state and request a redraw. This is the
    /// single sink for everything the service emits (see [`AppState::new`]).
    pub(super) fn apply_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Connecting { profile_id } => {
                self.connecting_id = Some(profile_id);
            }
            Event::HostKeyPrompt {
                host,
                fingerprint,
                kind,
            } => {
                self.host_key_prompt = Some(HostKeyPrompt {
                    host: host.into(),
                    fingerprint: fingerprint.into(),
                    kind,
                });
                self.arm_primary_focus();
            }
            Event::Connected { profile_id, home } => {
                self.host_key_prompt = None;
                self.used_stored_password = None;
                self.connection_lost = None;
                self.reconnect_attempt = None;
                self.reconnect_failed = false;
                // Persist the connect time so "Recent" ordering survives a restart.
                self.stamp_last_connected(&profile_id, cx);
                self.enter_browser(profile_id, home, cx);
            }
            Event::DirListing { path, entries } => {
                // Drop a listing for a directory we've since navigated away from.
                if path == self.cwd {
                    self.set_listing(entries.into_iter().map(EntryRow::new).collect());
                    self.listing_loading = false;
                    // Land on the entry a search hit opened, now that it's listed.
                    if let Some(name) = self.pending_select.take() {
                        if self.listing.iter().any(|r| r.entry.name == name.as_ref()) {
                            self.selected.insert(name);
                        }
                    }
                }
            }
            // A streamed batch of tree-search matches. Append it under the matching
            // token; a stale token (superseded search) is ignored.
            Event::SearchResult {
                token,
                hits,
                done,
                truncated,
            } => {
                if let Some(search) = self.search.as_mut() {
                    if search.token == token {
                        if !hits.is_empty() {
                            // Between frames the render closures are dropped, so the
                            // `Rc` is uniquely held and this extends in place.
                            Rc::make_mut(&mut search.hits)
                                .extend(hits.into_iter().map(SearchRow::from_hit));
                        }
                        search.done = done;
                        search.truncated = truncated;
                    }
                }
            }
            // A clicked symlink was followed: navigate into a directory target,
            // otherwise treat it as a file and download it.
            Event::SymlinkResolved { path, is_dir } => {
                if is_dir {
                    self.go_to_path(path, true, cx);
                } else {
                    let name = path.file_name().unwrap_or("download").to_string();
                    self.download_remote_file(path, name, cx);
                }
            }
            // The transport dropped: keep the last listing visible, drop the
            // online state, and show the reconnect banner. In-flight transfers
            // arrive as their own `TransferDone(Failed)` events.
            Event::ConnectionLost { profile_id, reason } => {
                if self.active_id.as_deref() == Some(profile_id.as_str()) {
                    self.online_id = None;
                    self.connecting_id = None;
                    self.listing_loading = false;
                    self.reconnect_attempt = None;
                    self.reconnect_failed = false;
                    self.connection_lost = Some(if reason.is_empty() {
                        "Connection lost".into()
                    } else {
                        reason.into()
                    });
                }
            }
            // The service is auto-reconnecting after a loss: reflect the attempt in
            // the banner (which offers Cancel instead of a manual Reconnect).
            Event::Reconnecting {
                profile_id,
                attempt,
                next_in: _,
            } => {
                if self.active_id.as_deref() == Some(profile_id.as_str()) {
                    self.reconnect_attempt = Some(attempt);
                    self.reconnect_failed = false;
                    self.connecting_id = None;
                    if self.connection_lost.is_none() {
                        self.connection_lost = Some("Connection lost".into());
                    }
                }
            }
            // Auto-reconnect gave up: the banner flips to "Reconnect failed" with a
            // manual Reconnect.
            Event::ReconnectFailed { profile_id, reason } => {
                if self.active_id.as_deref() == Some(profile_id.as_str()) {
                    self.reconnect_attempt = None;
                    self.reconnect_failed = true;
                    self.connection_lost = Some(if reason.is_empty() {
                        "Connection lost".into()
                    } else {
                        reason.into()
                    });
                }
            }
            Event::TestResult {
                profile_id,
                ok,
                message,
            } => {
                if let Some(editor) = self.editor.as_mut() {
                    if editor.id == profile_id {
                        editor.testing = false;
                        editor.test_status = Some(TestStatus {
                            ok,
                            message: message.into(),
                        });
                    }
                }
            }
            Event::FileOpDone { op, message } => {
                // Refresh the listing only for the mutating ops; transfers feed
                // the dock and refresh via `TransferDone` instead.
                self.push_toast(message, ToastVariant::Success, cx);
                if !matches!(op, FileOp::Download) {
                    self.selected.clear();
                    self.reload_listing(cx);
                }
            }
            Event::TransferQueued {
                id,
                direction,
                kind,
                remote,
                local,
            } => {
                // Link a drag-out promise to its transfer id (no-op otherwise).
                self.drag_downloads.note_queued(id, &local);
                self.transfers.push(TransferVm {
                    transfer: Transfer {
                        id,
                        direction,
                        kind,
                        remote_path: remote,
                        local_path: local,
                        total_bytes: None,
                        transferred_bytes: 0,
                        status: TransferStatus::Queued,
                    },
                    speed_bps: None,
                    error: None,
                    report: None,
                    report_expanded: false,
                });
            }
            Event::TransferCollision {
                id,
                direction,
                is_dir,
                remote,
                local,
                existing_size,
            } => {
                // Mark the dock row parked, then queue the prompt.
                let (name, path) = match direction {
                    TransferDirection::Upload => (
                        remote.file_name().unwrap_or("/").to_string(),
                        remote.as_str().to_string(),
                    ),
                    TransferDirection::Download => {
                        let name = std::path::Path::new(&local)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(local.as_str())
                            .to_string();
                        (name, local.clone())
                    }
                };
                if let Some(vm) = self.transfers.iter_mut().find(|t| t.transfer.id == id) {
                    vm.transfer.status = TransferStatus::AwaitingDecision;
                }
                self.pending_collisions.push(CollisionInfo {
                    id,
                    direction,
                    is_dir,
                    name: name.into(),
                    path: path.into(),
                    existing_size,
                });
                self.arm_primary_focus();
            }
            Event::TransferStarted { id, total } => {
                if let Some(vm) = self.transfers.iter_mut().find(|t| t.transfer.id == id) {
                    vm.transfer.status = TransferStatus::Running;
                    vm.transfer.total_bytes = total;
                }
            }
            // Ignore a progress sample for a row no longer Running: a late tick
            // can arrive after TransferDone.
            Event::TransferProgress {
                id,
                transferred,
                speed_bps,
            } => {
                if let Some(vm) = self.transfers.iter_mut().find(|t| t.transfer.id == id) {
                    if vm.transfer.status == TransferStatus::Running {
                        vm.transfer.transferred_bytes = transferred;
                        vm.speed_bps = Some(speed_bps);
                    }
                }
            }
            // Terminal state: keep the row so the Completed/Failed tabs populate;
            // on a completed upload into the current directory, refresh the listing.
            Event::TransferDone {
                id,
                status,
                message,
                report,
            } => {
                // Release any drag-out promise waiting on this transfer (no-op
                // otherwise), unblocking the OS callback that drives the drop.
                self.drag_downloads
                    .note_done(id, status, message.as_deref());
                let cwd = self.cwd.clone();
                // A completed folder transfer may carry a "N skipped/failed" note.
                let completed_note = (status == TransferStatus::Completed)
                    .then(|| message.clone())
                    .flatten();
                let mut refresh = false;
                if let Some(vm) = self.transfers.iter_mut().find(|t| t.transfer.id == id) {
                    vm.transfer.status = status;
                    vm.speed_bps = None;
                    // The per-entry detail behind a completed-with-issues folder.
                    vm.report = report;
                    match status {
                        TransferStatus::Completed => {
                            // Snap the bar to 100% even if no final sample landed.
                            if let Some(total) = vm.transfer.total_bytes {
                                vm.transfer.transferred_bytes = total;
                            }
                            // An upload into the current directory (file or folder
                            // root) refreshes the listing so the new entry shows.
                            refresh = vm.transfer.direction == TransferDirection::Upload
                                && vm.transfer.remote_path.parent().as_ref() == Some(&cwd);
                        }
                        TransferStatus::Failed => {
                            vm.error = message.map(SharedString::from);
                        }
                        _ => {}
                    }
                }
                if let Some(note) = completed_note {
                    self.push_toast(format!("Folder finished - {note}"), ToastVariant::Info, cx);
                }
                if refresh {
                    self.reload_listing(cx);
                }
            }
            // A transfer was paused by a connection loss: mark it Interrupted and
            // retain its watermark so the dock keeps the progress bar (it resumes
            // on reconnect). A drag-out promise can't resume gracefully, so resolve
            // its slot now to avoid hanging the OS drop.
            Event::TransferInterrupted { id, transferred } => {
                self.drag_downloads
                    .note_done(id, TransferStatus::Cancelled, None);
                if let Some(vm) = self.transfers.iter_mut().find(|t| t.transfer.id == id) {
                    vm.transfer.status = TransferStatus::Interrupted;
                    vm.speed_bps = None;
                    if transferred > 0 {
                        vm.transfer.transferred_bytes = transferred;
                    }
                }
            }
            Event::Error { message } => {
                let stale = self.used_stored_password.take();
                let connecting = self.connecting_id.take();
                self.host_key_prompt = None;
                self.listing_loading = false;
                self.push_toast(message.clone(), ToastVariant::Error, cx);
                // A stored password that fails auth is likely stale - re-open the
                // prompt so the user can correct (and overwrite) it.
                if message.contains("authentication failed") {
                    if let Some(id) = stale {
                        if let Some(conn) = self.connections.iter().find(|c| c.profile.id == id) {
                            let profile = conn.profile.clone();
                            self.show_password_prompt(profile, cx);
                        }
                    }
                // An encrypted key with no/wrong passphrase - prompt for it.
                } else if message.contains("key requires a passphrase") {
                    if let Some(id) = connecting {
                        if let Some(conn) = self.connections.iter().find(|c| c.profile.id == id) {
                            let profile = conn.profile.clone();
                            self.show_passphrase_prompt(profile, cx);
                        }
                    }
                }
            }
            // Surface a deferred startup error (e.g. malformed profiles.toml).
            Event::Ready => {
                if let Some(err) = self.startup_error.take() {
                    self.push_toast(err, ToastVariant::Error, cx);
                }
            }
            Event::Stopped => {}
            _ => {}
        }
        cx.notify();
    }

    /// Stamp a profile's `last_connected` to now and persist it, then refresh the
    /// in-memory connection list (for the "Recent" labels/ordering).
    pub(super) fn stamp_last_connected(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        if let Ok(Some(mut profile)) = self.store.get(profile_id) {
            profile.last_connected = Some(OffsetDateTime::now_utc());
            if let Err(err) = self.store.save(&profile) {
                self.push_toast(err.to_string(), ToastVariant::Error, cx);
                return;
            }
            self.reload_connections(cx);
        }
    }
}
