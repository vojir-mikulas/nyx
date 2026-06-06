// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! [`AppState`] — the single source of truth for the M1 app shell.
//!
//! One root `Entity<AppState>` holds all mutable state plus the interaction
//! logic (navigation, sort, filter, selection, dock). Views are `RenderOnce`
//! helpers that read a `&AppState` and emit elements; only the filter
//! [`TextInput`] is its own entity (it needs focus/IME state). Derived getters
//! ([`visible_entries`](AppState::visible_entries), [`dock_rows`](AppState::dock_rows))
//! compute from the fixtures with no cached duplicate state, so M2 can swap the
//! fixture source for real events with no logic change.

pub mod fixtures;
pub mod models;

use std::collections::HashSet;
use std::time::Duration;

use futures::StreamExt;
use gpui::{prelude::*, App, Context, Entity, SharedString};
use nyx_core::TransferStatus;
use nyx_service::{Command, Event, Secret, ServiceHandle};
use nyx_ui::{TextInput, ToastVariant};

use models::{ConnectionVm, Density, DockTab, EntryRow, SortKey, TransferVm};

/// Which top-level screen the main column shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    /// The welcome / connection-manager screen.
    Welcome,
    /// The file browser for the active connection.
    Browse,
}

/// A transient toast notification.
pub struct ToastMsg {
    /// The message text.
    pub message: SharedString,
    /// The toast variant (status color).
    pub variant: ToastVariant,
    /// Monotonic id so a stale auto-dismiss does not clear a newer toast.
    pub id: u64,
}

/// The password prompt shown before a connection is attempted (M2).
///
/// M3 replaces this with a keyring lookup that only prompts on a miss; the
/// password is read straight out of the masked input into the `Connect` command
/// and never stored on `AppState`.
pub struct PasswordPrompt {
    /// The profile being connected to.
    pub profile_id: String,
    /// Display name (modal title).
    pub profile_name: SharedString,
    /// `user@host:port` shown under the title.
    pub host_label: SharedString,
    /// The masked password field.
    pub input: Entity<TextInput>,
}

/// A pending host-key trust-on-first-use prompt (an unknown key was presented).
pub struct HostKeyPrompt {
    /// The host the key belongs to.
    pub host: SharedString,
    /// The SHA-256 fingerprint, e.g. `SHA256:…`.
    pub fingerprint: SharedString,
}

/// The whole application's mutable state.
pub struct AppState {
    /// Current top-level screen.
    pub view: View,
    /// All connection profiles (saved + recent).
    pub connections: Vec<ConnectionVm>,
    /// The connection currently open in the browser.
    pub active_id: Option<String>,
    /// The connection shown as connected (fake: equals `active_id`).
    pub online_id: Option<String>,

    // --- browser ---
    /// Current path segments, e.g. `["var", "www"]`.
    pub cwd: Vec<SharedString>,
    /// Back/forward navigation stack.
    pub history: Vec<Vec<SharedString>>,
    /// Cursor into `history`.
    pub history_ix: usize,
    /// Fixture listing for the current `cwd`.
    pub listing: Vec<EntryRow>,
    /// The stateful filter box.
    pub filter: Entity<TextInput>,
    /// Active sort: `(key, ascending)`.
    pub sort: (SortKey, bool),
    /// Selected entry names.
    pub selected: HashSet<SharedString>,

    // --- transfer dock ---
    /// Whether the dock body is expanded.
    pub dock_open: bool,
    /// Active dock filter tab.
    pub dock_tab: DockTab,
    /// All transfers.
    pub transfers: Vec<TransferVm>,

    // --- chrome / tweaks ---
    /// Whether the sidebar is shown.
    pub sidebar_open: bool,
    /// Whether the tweaks modal is open.
    pub tweaks_open: bool,
    /// File-row density (exercises `Table::row_height`).
    pub density: Density,
    /// Whether the permissions column is shown.
    pub show_perms: bool,
    /// The current toast, if any.
    pub toast: Option<ToastMsg>,
    /// Monotonic toast id source.
    next_toast_id: u64,

    // --- backend bridge (M2) ---
    /// Handle to the backend thread (dropped on app exit → graceful shutdown).
    service: ServiceHandle,
    /// The profile id of an in-flight connection attempt, if any.
    pub connecting_id: Option<String>,
    /// A pending password prompt (shown before connecting).
    pub password_prompt: Option<PasswordPrompt>,
    /// A pending host-key trust prompt (unknown key).
    pub host_key_prompt: Option<HostKeyPrompt>,
    /// Whether a directory listing is in flight (drives the loading hint).
    pub listing_loading: bool,
}

impl AppState {
    /// Build the initial state: welcome screen, connections loaded, nothing open.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| TextInput::new(cx).with_placeholder("Filter this folder…"));
        // Re-render whenever the filter text changes.
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();

        // Spawn the backend thread and drain its events into this entity. The
        // drain runs on the GPUI foreground executor: `next().await` yields, so it
        // never blocks the UI. This is the single Tokio↔GPUI bridge (M2); later
        // milestones only add event variants, never another bridge.
        let (service, mut events) = nyx_service::spawn();
        cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this
                    .update(cx, |state, cx| state.apply_event(event, cx))
                    .is_err()
                {
                    break; // entity gone → app is closing
                }
            }
        })
        .detach();

        Self {
            view: View::Welcome,
            connections: fixtures::fake_connections(),
            active_id: None,
            online_id: None,
            cwd: Vec::new(),
            history: vec![Vec::new()],
            history_ix: 0,
            listing: Vec::new(),
            filter,
            sort: (SortKey::Name, true),
            selected: HashSet::new(),
            dock_open: true,
            dock_tab: DockTab::All,
            transfers: Vec::new(),
            sidebar_open: true,
            tweaks_open: false,
            density: Density::Comfortable,
            show_perms: true,
            toast: None,
            next_toast_id: 0,
            service,
            connecting_id: None,
            password_prompt: None,
            host_key_prompt: None,
            listing_loading: false,
        }
    }

    // --- connections ------------------------------------------------------

    /// All connections (the "Saved" group).
    pub fn connections_all(&self) -> Vec<&ConnectionVm> {
        self.connections.iter().collect()
    }

    /// The connection currently open in the browser, if any.
    pub fn active_conn(&self) -> Option<&ConnectionVm> {
        let id = self.active_id.as_deref()?;
        self.connections.iter().find(|c| c.profile.id == id)
    }

    /// Begin opening a connection: show the password prompt.
    ///
    /// The actual `Connect` command is sent from [`confirm_password`](Self::confirm_password)
    /// once the user supplies a secret. M3 swaps the prompt for a keyring lookup.
    pub fn open_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.connections.iter().find(|c| c.profile.id == id) else {
            return;
        };
        let profile_name = conn.profile.name.clone();
        let host_label = conn.user_host_port();
        let input = cx.new(|cx| TextInput::new(cx).with_placeholder("Password").obscured());
        self.password_prompt = Some(PasswordPrompt {
            profile_id: id.to_string(),
            profile_name: profile_name.into(),
            host_label: host_label.into(),
            input,
        });
    }

    /// Submit the password prompt: send a `Connect` command to the backend.
    pub fn confirm_password(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.password_prompt.take() else {
            return;
        };
        let password = prompt.input.read(cx).content().to_string();
        let Some(conn) = self
            .connections
            .iter()
            .find(|c| c.profile.id == prompt.profile_id)
        else {
            return;
        };
        let profile = conn.profile.clone();
        self.connecting_id = Some(prompt.profile_id.clone());
        let sent = self.service.send(Command::Connect {
            profile,
            password: Secret::new(password),
        });
        if !sent {
            self.connecting_id = None;
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Dismiss the password prompt without connecting.
    pub fn cancel_password(&mut self) {
        self.password_prompt = None;
    }

    /// Trust the pending host key and continue connecting.
    pub fn trust_host_key(&mut self) {
        self.host_key_prompt = None;
        self.service.send(Command::HostKeyDecision { accept: true });
    }

    /// Reject the pending host key and abort the connection.
    pub fn reject_host_key(&mut self) {
        self.host_key_prompt = None;
        self.service
            .send(Command::HostKeyDecision { accept: false });
    }

    /// Close the active connection and return to the welcome screen.
    pub fn disconnect(&mut self) {
        self.service.send(Command::Disconnect);
        self.active_id = None;
        self.online_id = None;
        self.connecting_id = None;
        self.view = View::Welcome;
        self.transfers.clear();
        self.listing.clear();
        self.selected.clear();
        self.listing_loading = false;
    }

    /// Apply a backend [`Event`] to the state and request a redraw. This is the
    /// single sink for everything the service emits (see [`AppState::new`]).
    fn apply_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Connecting { profile_id } => {
                self.connecting_id = Some(profile_id);
            }
            Event::HostKeyPrompt { host, fingerprint } => {
                self.host_key_prompt = Some(HostKeyPrompt {
                    host: host.into(),
                    fingerprint: fingerprint.into(),
                });
            }
            Event::Connected { profile_id } => {
                self.host_key_prompt = None;
                self.enter_browser(profile_id, cx);
            }
            Event::DirListing { path, entries } => {
                // Drop a listing for a directory we've since navigated away from.
                if path == self.current_path() {
                    self.listing = entries.into_iter().map(EntryRow::new).collect();
                    self.listing_loading = false;
                }
            }
            Event::Error { message } => {
                self.host_key_prompt = None;
                self.connecting_id = None;
                self.listing_loading = false;
                self.push_toast(message, ToastVariant::Error, cx);
            }
            // Lifecycle pings (and any future variants) need no UI change.
            Event::Ready | Event::Stopped => {}
            _ => {}
        }
        cx.notify();
    }

    /// Enter the browser for a freshly-connected profile and list its root.
    fn enter_browser(&mut self, profile_id: String, cx: &mut Context<Self>) {
        let root = self
            .connections
            .iter()
            .find(|c| c.profile.id == profile_id)
            .and_then(|c| c.profile.remote_path.as_deref())
            .map(path_segments)
            .unwrap_or_default();

        self.active_id = Some(profile_id.clone());
        self.online_id = Some(profile_id.clone());
        self.connecting_id = None;
        self.view = View::Browse;
        self.cwd = root.clone();
        self.history = vec![root];
        self.history_ix = 0;
        self.selected.clear();
        self.filter
            .update(cx, |input, cx| input.set_content("", cx));
        self.dock_open = true;
        // Transfers are still fixtures until M5; seed the prod box's dock.
        self.transfers = if profile_id == "prod" {
            fixtures::fake_transfers()
        } else {
            Vec::new()
        };
        self.reload_listing(cx);
    }

    // --- navigation -------------------------------------------------------

    /// The current working directory as an absolute remote path (`"/"` at root).
    fn current_path(&self) -> String {
        if self.cwd.is_empty() {
            "/".to_string()
        } else {
            let joined = self
                .cwd
                .iter()
                .map(|s| s.as_ref())
                .collect::<Vec<_>>()
                .join("/");
            format!("/{joined}")
        }
    }

    /// Request a listing for the current `cwd` from the backend. The result
    /// arrives asynchronously as an [`Event::DirListing`].
    fn reload_listing(&mut self, cx: &mut Context<Self>) {
        self.listing.clear();
        self.listing_loading = true;
        if !self.service.send(Command::ListDir {
            path: self.current_path(),
        }) {
            self.listing_loading = false;
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Navigate to a path, optionally pushing onto the history stack.
    fn go_to_path(&mut self, segs: Vec<SharedString>, push_history: bool, cx: &mut Context<Self>) {
        self.cwd = segs.clone();
        self.selected.clear();
        self.filter
            .update(cx, |input, cx| input.set_content("", cx));
        if push_history {
            self.history.truncate(self.history_ix + 1);
            self.history.push(segs);
            self.history_ix = self.history.len() - 1;
        }
        self.reload_listing(cx);
    }

    /// Open a child directory by name.
    pub fn open_dir(&mut self, name: &SharedString, cx: &mut Context<Self>) {
        let mut segs = self.cwd.clone();
        segs.push(name.clone());
        self.go_to_path(segs, true, cx);
    }

    /// Jump to the `n`-th breadcrumb (0 = root).
    pub fn nav_crumb(&mut self, n: usize, cx: &mut Context<Self>) {
        let segs = self.cwd[..n.min(self.cwd.len())].to_vec();
        self.go_to_path(segs, true, cx);
    }

    /// Go up one directory level.
    pub fn go_up(&mut self, cx: &mut Context<Self>) {
        if self.cwd.is_empty() {
            return;
        }
        let segs = self.cwd[..self.cwd.len() - 1].to_vec();
        self.go_to_path(segs, true, cx);
    }

    /// Whether back navigation is available.
    pub fn can_back(&self) -> bool {
        self.history_ix > 0
    }

    /// Whether forward navigation is available.
    pub fn can_forward(&self) -> bool {
        self.history_ix + 1 < self.history.len()
    }

    /// Step back in history.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        if !self.can_back() {
            return;
        }
        self.history_ix -= 1;
        let segs = self.history[self.history_ix].clone();
        self.go_to_path(segs, false, cx);
    }

    /// Step forward in history.
    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if !self.can_forward() {
            return;
        }
        self.history_ix += 1;
        let segs = self.history[self.history_ix].clone();
        self.go_to_path(segs, false, cx);
    }

    /// Refresh the current listing.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload_listing(cx);
    }

    // --- sort / filter / selection ---------------------------------------

    /// Cycle the sort for a clicked column header.
    pub fn toggle_sort(&mut self, column: usize) {
        let Some(key) = SortKey::from_column(column) else {
            return;
        };
        self.sort = if self.sort.0 == key {
            (key, !self.sort.1)
        } else {
            (key, true)
        };
    }

    /// The current filter text (lower-cased compare happens in the getter).
    pub fn filter_text(&self, cx: &App) -> String {
        self.filter.read(cx).content().to_string()
    }

    /// The entries to display: filtered by name, then sorted (folders first).
    pub fn visible_entries(&self, cx: &App) -> Vec<&EntryRow> {
        let filter = self.filter_text(cx).trim().to_lowercase();
        let mut rows: Vec<&EntryRow> = self
            .listing
            .iter()
            .filter(|row| filter.is_empty() || row.entry.name.to_lowercase().contains(&filter))
            .collect();

        let (key, asc) = self.sort;
        rows.sort_by(|a, b| {
            // Directories always sort before files.
            let dir_order = b.entry.is_dir.cmp(&a.entry.is_dir);
            if dir_order != std::cmp::Ordering::Equal {
                return dir_order;
            }
            let ord = match key {
                SortKey::Name => a
                    .entry
                    .name
                    .to_lowercase()
                    .cmp(&b.entry.name.to_lowercase()),
                SortKey::Size => a.entry.size.cmp(&b.entry.size),
                SortKey::Modified => a.entry.modified.cmp(&b.entry.modified),
                SortKey::Kind => a.type_label.cmp(&b.type_label),
            };
            if asc {
                ord
            } else {
                ord.reverse()
            }
        });
        rows
    }

    /// Apply a row click: plain click replaces, cmd/ctrl-click toggles.
    pub fn select(&mut self, name: SharedString, additive: bool) {
        if additive {
            if !self.selected.remove(&name) {
                self.selected.insert(name);
            }
        } else {
            self.selected.clear();
            self.selected.insert(name);
        }
    }

    /// Count of selected entries.
    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// Count of entries in the current listing.
    pub fn item_count(&self) -> usize {
        self.listing.len()
    }

    // --- transfer dock ----------------------------------------------------

    /// The transfers visible under the active dock tab.
    pub fn dock_rows(&self) -> Vec<&TransferVm> {
        self.transfers
            .iter()
            .filter(|t| self.dock_tab.matches(t.transfer.status))
            .collect()
    }

    /// `(all, active, completed, failed)` dock counts.
    pub fn dock_counts(&self) -> (usize, usize, usize, usize) {
        let mut counts = (self.transfers.len(), 0, 0, 0);
        for t in &self.transfers {
            match t.transfer.status {
                TransferStatus::Running | TransferStatus::Queued => counts.1 += 1,
                TransferStatus::Completed => counts.2 += 1,
                TransferStatus::Failed => counts.3 += 1,
                TransferStatus::Cancelled => {}
            }
        }
        counts
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

    /// Clear finished (completed / failed / cancelled) transfers from the dock.
    pub fn clear_finished(&mut self) {
        self.transfers.retain(|t| {
            matches!(
                t.transfer.status,
                TransferStatus::Running | TransferStatus::Queued
            )
        });
    }

    // --- toasts -----------------------------------------------------------

    /// Show a toast that auto-dismisses after a short delay.
    pub fn push_toast(
        &mut self,
        message: impl Into<SharedString>,
        variant: ToastVariant,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toast = Some(ToastMsg {
            message: message.into(),
            variant,
            id,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(2600))
                .await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|t| t.id == id) {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

/// Split a remote path into non-empty segments.
fn path_segments(path: &str) -> Vec<SharedString> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(SharedString::from)
        .collect()
}
