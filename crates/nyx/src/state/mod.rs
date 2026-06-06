// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! [`AppState`] — the single source of truth for the M1 app shell.
//!
//! One root `Entity<AppState>` holds all mutable state plus the interaction
//! logic (navigation, sort, filter, selection, dock). Views are `RenderOnce`
//! helpers that read a `&AppState` and emit elements; only the filter
//! [`TextInput`] is its own entity (it needs focus/IME state). Derived getters
//! ([`visible_entries`](AppState::visible_entries), [`dock_rows`](AppState::dock_rows))
//! compute from the live state with no cached duplicate, so the source can be a
//! real backend event with no logic change.

pub mod models;

use std::collections::HashSet;
use std::time::Duration;

use futures::StreamExt;
use gpui::{
    prelude::*, App, ClipboardItem, Context, Entity, FocusHandle, PathPromptOptions, Pixels, Point,
    SharedString,
};
use nyx_core::{Protocol, Transfer, TransferDirection, TransferId, TransferStatus};
use nyx_keyring::{CredentialStore, OsKeyring};
use nyx_profile::{FileProfileStore, Profile, ProfileColor, ProfileStore};
use nyx_service::{Command, Event, FileOp, Secret, ServiceHandle};
use nyx_ui::{TextInput, TextInputEvent, ToastVariant};
use time::OffsetDateTime;

use models::{AccentKind, ConnectionVm, Density, DockTab, EntryRow, SortKey, TransferVm};

/// The keychain service name (every password is addressed `("nyx", profile.id)`).
const KEYCHAIN_SERVICE: &str = "nyx";

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
    /// Whether to write the entered password to the keychain on connect.
    pub save_to_keychain: bool,
}

/// A pending host-key trust-on-first-use prompt (an unknown key was presented).
pub struct HostKeyPrompt {
    /// The host the key belongs to.
    pub host: SharedString,
    /// The SHA-256 fingerprint, e.g. `SHA256:…`.
    pub fingerprint: SharedString,
}

/// The inline result of an editor "Test connection" probe.
pub struct TestStatus {
    /// Whether the probe succeeded.
    pub ok: bool,
    /// The credential-free status / error message.
    pub message: SharedString,
}

/// The connection editor modal's mutable state (create or edit a profile).
pub struct ConnectionEditor {
    /// The profile id — freshly generated on create, preserved on edit.
    pub id: String,
    /// `true` when creating (vs. editing an existing profile).
    pub is_new: bool,
    /// Display-name field.
    pub name: Entity<TextInput>,
    /// Host field.
    pub host: Entity<TextInput>,
    /// Port field (numeric; default from the protocol when blank).
    pub port: Entity<TextInput>,
    /// Username field.
    pub username: Entity<TextInput>,
    /// Optional remote-path field.
    pub remote_path: Entity<TextInput>,
    /// Password field (obscured). On edit, blank means "keep the stored secret".
    pub password: Entity<TextInput>,
    /// Selected protocol.
    pub protocol: Protocol,
    /// Selected accent color.
    pub color: AccentKind,
    /// Inline test-connection status, if a probe has reported.
    pub test_status: Option<TestStatus>,
    /// Whether a test probe is currently in flight.
    pub testing: bool,
}

/// A pending right-click context menu on a sidebar connection row.
pub struct RowMenu {
    /// The profile the menu acts on.
    pub profile_id: String,
    /// The display name (for the confirm copy).
    pub profile_name: SharedString,
    /// Where the menu is anchored (the cursor position).
    pub position: Point<Pixels>,
}

/// A pending "remove connection?" confirmation.
pub struct DeleteConfirm {
    /// The profile to delete.
    pub profile_id: String,
    /// The display name shown in the prompt.
    pub profile_name: SharedString,
}

/// A pending right-click context menu on a browser file row (Download / Rename /
/// Delete / Copy path). Delete/Download operate on the whole selection; Rename /
/// Copy path target the clicked row (plan M4 D7).
pub struct FileMenu {
    /// The clicked row's name (the single-target ops act on this).
    pub name: SharedString,
    /// Whether the clicked row is a directory (Download is unsupported for dirs).
    pub is_dir: bool,
    /// Where the menu is anchored (the cursor position).
    pub position: Point<Pixels>,
}

/// Which mutating op a submitted [`InputPrompt`] performs.
#[derive(Clone)]
pub enum InputAction {
    /// Create a new folder in the current directory.
    NewFolder,
    /// Rename `original` (a name in the current directory) to the entered value.
    Rename {
        /// The current name of the entry being renamed.
        original: SharedString,
    },
}

/// A reusable single-field input modal — shared by **New folder** (blank) and
/// **Rename** (prefilled). Validated on submit (non-empty, no `/`).
pub struct InputPrompt {
    /// Modal title.
    pub title: SharedString,
    /// The field's label.
    pub label: SharedString,
    /// The submit button's label ("Create" / "Rename").
    pub submit_label: SharedString,
    /// The text field.
    pub input: Entity<TextInput>,
    /// What submitting does.
    pub action: InputAction,
}

/// A pending "delete these files?" confirmation. Each entry carries its `is_dir`
/// flag so the issued `Remove` commands pick file vs. recursive delete.
pub struct FileDeleteConfirm {
    /// The selected entries to delete, as `(name, is_dir)`.
    pub entries: Vec<(SharedString, bool)>,
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
    /// Whether the sidebar's **Recent** group is collapsed (session-only; not
    /// persisted to the profile store for MVP — plan M6 D6).
    pub recent_collapsed: bool,
    /// Focus handle for the file browser table, so the `"Browser"` key context
    /// (Enter / Backspace / F2 / Delete) has somewhere to dispatch (plan M6 D11).
    pub browser_focus: FocusHandle,
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

    // --- persistence (M3) ---
    /// On-disk profile store (the source of `connections`).
    store: FileProfileStore,
    /// OS keychain for connection passwords (addressed by profile id).
    keyring: OsKeyring,
    /// A startup error to surface once the backend is `Ready` (e.g. a malformed
    /// `profiles.toml`); kept so construction can't push a toast.
    startup_error: Option<SharedString>,

    // --- backend bridge (M2) ---
    /// Handle to the backend thread (dropped on app exit → graceful shutdown).
    service: ServiceHandle,
    /// The profile id of an in-flight connection attempt, if any.
    pub connecting_id: Option<String>,
    /// The profile id whose connect used a *stored* password — set so an auth
    /// failure can re-open the prompt to correct a stale keychain entry (D5.3).
    used_stored_password: Option<String>,
    /// A pending password prompt (shown before connecting).
    pub password_prompt: Option<PasswordPrompt>,
    /// A pending host-key trust prompt (unknown key).
    pub host_key_prompt: Option<HostKeyPrompt>,
    /// The connection editor modal, if open.
    pub editor: Option<ConnectionEditor>,
    /// A pending sidebar row context menu, if open.
    pub row_menu: Option<RowMenu>,
    /// A pending delete confirmation, if open.
    pub delete_confirm: Option<DeleteConfirm>,
    /// A pending browser file-row context menu, if open (M4).
    pub file_menu: Option<FileMenu>,
    /// A pending New-folder / Rename input modal, if open (M4).
    pub input_prompt: Option<InputPrompt>,
    /// A pending file-delete confirmation, if open (M4).
    pub file_delete: Option<FileDeleteConfirm>,
    /// Whether a directory listing is in flight (drives the loading hint).
    pub listing_loading: bool,
}

impl AppState {
    /// Build the initial state: welcome screen, connections loaded, nothing open.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| TextInput::new(cx).with_placeholder("Filter this folder…"));
        // Re-render whenever the filter text changes.
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        let browser_focus = cx.focus_handle();

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

        // Open the on-disk store and load the saved connections. A missing file
        // is an empty list (first run); a malformed one is surfaced as a toast
        // once the backend is `Ready` (construction can't toast yet) — and the
        // store is *not* overwritten, so the user can fix the file.
        let store = FileProfileStore::open_default()
            .unwrap_or_else(|_| FileProfileStore::with_path("profiles.toml"));
        let (connections, startup_error) = match store.list() {
            Ok(profiles) => (
                profiles
                    .into_iter()
                    .map(ConnectionVm::from_profile)
                    .collect(),
                None,
            ),
            Err(err) => (Vec::new(), Some(SharedString::from(err.to_string()))),
        };

        Self {
            view: View::Welcome,
            connections,
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
            recent_collapsed: false,
            browser_focus,
            tweaks_open: false,
            density: Density::Comfortable,
            show_perms: true,
            toast: None,
            next_toast_id: 0,
            store,
            keyring: OsKeyring::new(),
            startup_error,
            service,
            connecting_id: None,
            used_stored_password: None,
            password_prompt: None,
            host_key_prompt: None,
            editor: None,
            row_menu: None,
            delete_confirm: None,
            file_menu: None,
            input_prompt: None,
            file_delete: None,
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

    /// Reload `connections` from the on-disk store (after a save/delete/stamp).
    fn reload_connections(&mut self, cx: &mut Context<Self>) {
        match self.store.list() {
            Ok(profiles) => {
                self.connections = profiles
                    .into_iter()
                    .map(ConnectionVm::from_profile)
                    .collect();
            }
            Err(err) => self.push_toast(err.to_string(), ToastVariant::Error, cx),
        }
    }

    /// Connections that count as "Recent", newest first.
    pub fn recent_connections(&self) -> Vec<&ConnectionVm> {
        let mut recents: Vec<&ConnectionVm> =
            self.connections.iter().filter(|c| c.is_recent).collect();
        recents.sort_by_key(|c| std::cmp::Reverse(c.profile.last_connected));
        recents
    }

    /// Begin opening a connection: look the password up in the keychain
    /// off-thread, then either connect straight through (hit) or prompt (miss).
    ///
    /// `connecting_id` is set up-front so the UI shows progress while the
    /// (potentially dialog-popping) keychain lookup runs on a background thread —
    /// the GPUI thread never blocks on it (plan M3 D3/D5).
    pub fn open_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.connections.iter().find(|c| c.profile.id == id) else {
            return;
        };
        let profile = conn.profile.clone();
        self.connecting_id = Some(id.to_string());

        let keyring = self.keyring;
        let lookup_id = id.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { keyring.get_password(KEYCHAIN_SERVICE, &lookup_id) })
                .await;
            this.update(cx, |this, cx| {
                this.on_password_lookup(profile, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Apply the result of the keychain lookup started by [`open_connection`].
    fn on_password_lookup(
        &mut self,
        profile: Profile,
        result: nyx_core::Result<Option<String>>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(password)) => self.send_connect(profile, password, true, cx),
            Ok(None) => self.show_password_prompt(profile, cx),
            Err(err) => {
                self.connecting_id = None;
                self.push_toast(format!("Keychain error: {err}"), ToastVariant::Error, cx);
            }
        }
    }

    /// Show the password prompt for `profile` (a keychain miss, or a stale-secret
    /// re-prompt). The "Save to keychain" toggle defaults on.
    fn show_password_prompt(&mut self, profile: Profile, cx: &mut Context<Self>) {
        let host_label = format!("{}@{}:{}", profile.username, profile.host, profile.port);
        let input = cx.new(|cx| TextInput::new(cx).with_placeholder("Password").obscured());
        self.wire_input(&input, cx);
        self.password_prompt = Some(PasswordPrompt {
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone().into(),
            host_label: host_label.into(),
            input,
            save_to_keychain: true,
        });
    }

    /// Send a `Connect` command, tracking whether a *stored* password was used.
    fn send_connect(
        &mut self,
        profile: Profile,
        password: String,
        from_keychain: bool,
        cx: &mut Context<Self>,
    ) {
        let id = profile.id.clone();
        self.connecting_id = Some(id.clone());
        self.used_stored_password = from_keychain.then_some(id);
        let sent = self.service.send(Command::Connect {
            profile,
            password: Secret::new(password),
        });
        if !sent {
            self.connecting_id = None;
            self.used_stored_password = None;
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Submit the password prompt: optionally save the secret to the keychain
    /// (off-thread, *before* connecting so it persists even if connect fails),
    /// then send `Connect`.
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

        if prompt.save_to_keychain && !password.is_empty() {
            self.save_password_then_connect(profile, password, cx);
        } else {
            self.send_connect(profile, password, false, cx);
        }
    }

    /// Save a password to the keychain off-thread, then send `Connect` once the
    /// write completes (a failed save is a non-fatal toast — we still connect).
    fn save_password_then_connect(
        &mut self,
        profile: Profile,
        password: String,
        cx: &mut Context<Self>,
    ) {
        self.connecting_id = Some(profile.id.clone());
        let keyring = self.keyring;
        let id = profile.id.clone();
        let pw_for_save = password.clone();
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_executor()
                .spawn(async move { keyring.set_password(KEYCHAIN_SERVICE, &id, &pw_for_save) })
                .await;
            this.update(cx, |this, cx| {
                if saved.is_err() {
                    this.push_toast(
                        "Couldn't save password to keychain",
                        ToastVariant::Error,
                        cx,
                    );
                }
                // A freshly-typed password isn't a "stored" one for the
                // auth-retry heuristic, even though we just saved it.
                this.send_connect(profile, password, false, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Dismiss the password prompt without connecting.
    pub fn cancel_password(&mut self) {
        self.password_prompt = None;
        self.connecting_id = None;
        self.used_stored_password = None;
    }

    /// Toggle the password prompt's "Save to keychain" switch.
    pub fn set_password_save(&mut self, on: bool) {
        if let Some(prompt) = self.password_prompt.as_mut() {
            prompt.save_to_keychain = on;
        }
    }

    // --- text-input submit/cancel wiring (D3) ----------------------------

    /// Subscribe to a modal field's [`TextInputEvent`]s so Enter submits and Esc
    /// dismisses the modal it belongs to — without a mouse. The field can't know
    /// what "submit" means, so the dispatch is routed by *which* modal is open
    /// (they're mutually exclusive in practice). The filter box is deliberately
    /// **not** wired (plan M6 D3).
    fn wire_input(&self, input: &Entity<TextInput>, cx: &mut Context<Self>) {
        cx.subscribe(input, |this, _input, event, cx| {
            match event {
                TextInputEvent::Submit => this.submit_focused_modal(cx),
                TextInputEvent::Cancel => this.cancel_focused_modal(),
            }
            cx.notify();
        })
        .detach();
    }

    /// Route an Enter from a wired field to the open modal's primary action.
    fn submit_focused_modal(&mut self, cx: &mut Context<Self>) {
        if self.password_prompt.is_some() {
            self.confirm_password(cx);
        } else if self.input_prompt.is_some() {
            self.submit_input(cx);
        } else if self.editor.is_some() {
            self.save_editor(cx);
        }
    }

    /// Route an Esc from a wired field to the open modal's dismiss action.
    fn cancel_focused_modal(&mut self) {
        if self.password_prompt.is_some() {
            self.cancel_password();
        } else if self.input_prompt.is_some() {
            self.cancel_input();
        } else if self.editor.is_some() {
            self.close_editor();
        }
    }

    /// Toggle the sidebar **Recent** group's collapsed state (plan M6 D6).
    pub fn toggle_recent_collapsed(&mut self) {
        self.recent_collapsed = !self.recent_collapsed;
    }

    // --- connection editor + CRUD ----------------------------------------

    /// Open the editor in **Create** mode (a fresh id, blank form).
    pub fn open_editor_create(&mut self, cx: &mut Context<Self>) {
        self.row_menu = None;
        self.editor = Some(ConnectionEditor {
            id: Profile::new_id(),
            is_new: true,
            name: cx.new(|cx| TextInput::new(cx).with_placeholder("My server")),
            host: cx.new(|cx| TextInput::new(cx).with_placeholder("example.com")),
            port: cx.new(|cx| TextInput::new(cx).with_placeholder("22").with_content("22")),
            username: cx.new(|cx| TextInput::new(cx).with_placeholder("user")),
            remote_path: cx.new(|cx| TextInput::new(cx).with_placeholder("/var/www  (optional)")),
            password: cx.new(|cx| TextInput::new(cx).with_placeholder("Password").obscured()),
            protocol: Protocol::Sftp,
            color: AccentKind::Purple,
            test_status: None,
            testing: false,
        });
        self.wire_editor_inputs(cx);
    }

    /// Open the editor in **Edit** mode, prefilled from an existing profile. The
    /// password field stays blank (we never read the secret back out of the
    /// keychain to display it; blank on save means "keep the stored secret").
    pub fn open_editor_edit(&mut self, id: &str, cx: &mut Context<Self>) {
        self.row_menu = None;
        let Some(conn) = self.connections.iter().find(|c| c.profile.id == id) else {
            return;
        };
        let p = conn.profile.clone();
        self.editor = Some(ConnectionEditor {
            id: p.id.clone(),
            is_new: false,
            name: cx.new(|cx| TextInput::new(cx).with_content(p.name.clone())),
            host: cx.new(|cx| TextInput::new(cx).with_content(p.host.clone())),
            port: cx.new(|cx| TextInput::new(cx).with_content(p.port.to_string())),
            username: cx.new(|cx| TextInput::new(cx).with_content(p.username.clone())),
            remote_path: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("/var/www  (optional)")
                    .with_content(p.remote_path.clone().unwrap_or_default())
            }),
            password: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("Leave blank to keep current")
                    .obscured()
            }),
            protocol: p.protocol,
            color: AccentKind::from_profile_color(p.color),
            test_status: None,
            testing: false,
        });
        self.wire_editor_inputs(cx);
    }

    /// Wire every editor field's submit/cancel events (Enter saves, Esc closes)
    /// after the editor has been constructed (plan M6 D3).
    fn wire_editor_inputs(&self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let inputs = [
            editor.name.clone(),
            editor.host.clone(),
            editor.port.clone(),
            editor.username.clone(),
            editor.remote_path.clone(),
            editor.password.clone(),
        ];
        for input in &inputs {
            self.wire_input(input, cx);
        }
    }

    /// Close the editor without saving.
    pub fn close_editor(&mut self) {
        self.editor = None;
    }

    /// Change the editor's protocol, applying the new default port when the port
    /// field is blank or still holds the previous protocol's default.
    pub fn set_editor_protocol(&mut self, ix: usize, cx: &mut Context<Self>) {
        let new = match ix {
            1 => Protocol::Ftp,
            2 => Protocol::Ftps,
            _ => Protocol::Sftp,
        };
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let old = editor.protocol;
        editor.protocol = new;
        let port_input = editor.port.clone();
        let port_text = port_input.read(cx).content().to_string();
        let port_trim = port_text.trim();
        let holds_old_default = port_trim.parse::<u16>().ok() == Some(old.default_port());
        if port_trim.is_empty() || holds_old_default {
            port_input.update(cx, |input, cx| {
                input.set_content(new.default_port().to_string(), cx)
            });
        }
    }

    /// Change the editor's accent color by picker index.
    pub fn set_editor_color(&mut self, ix: usize) {
        if let Some(editor) = self.editor.as_mut() {
            editor.color = AccentKind::ALL.get(ix).copied().unwrap_or(AccentKind::Blue);
        }
    }

    /// Validate and save the editor's profile (and its password, if entered).
    pub fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let name = editor.name.read(cx).content().trim().to_string();
        let host = editor.host.read(cx).content().trim().to_string();
        let port_text = editor.port.read(cx).content().trim().to_string();
        let username = editor.username.read(cx).content().trim().to_string();
        let remote_path = editor.remote_path.read(cx).content().trim().to_string();
        let password = editor.password.read(cx).content().to_string();
        let protocol = editor.protocol;
        let color = editor.color;
        let id = editor.id.clone();
        let is_new = editor.is_new;

        if name.is_empty() || host.is_empty() || username.is_empty() {
            self.push_toast(
                "Name, host and username are required",
                ToastVariant::Error,
                cx,
            );
            return;
        }
        let port = if port_text.is_empty() {
            protocol.default_port()
        } else {
            match port_text.parse::<u16>() {
                Ok(port) => port,
                Err(_) => {
                    self.push_toast("Port must be a number (1–65535)", ToastVariant::Error, cx);
                    return;
                }
            }
        };

        // Preserve the existing last_connected across an edit.
        let last_connected = self
            .store
            .get(&id)
            .ok()
            .flatten()
            .and_then(|p| p.last_connected);
        let profile = Profile {
            id: id.clone(),
            name,
            protocol,
            host,
            port,
            username,
            remote_path: (!remote_path.is_empty()).then_some(remote_path),
            color: color.to_profile_color(),
            last_connected,
        };
        if let Err(err) = self.store.save(&profile) {
            self.push_toast(err.to_string(), ToastVariant::Error, cx);
            return;
        }
        if !password.is_empty() {
            self.keyring_set_async(id, password, cx);
        }
        self.reload_connections(cx);
        self.editor = None;
        self.push_toast(
            if is_new {
                "Connection created"
            } else {
                "Connection saved"
            },
            ToastVariant::Success,
            cx,
        );
    }

    /// Write a password to the keychain off-thread, toasting on failure.
    fn keyring_set_async(&self, id: String, password: String, cx: &mut Context<Self>) {
        let keyring = self.keyring;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { keyring.set_password(KEYCHAIN_SERVICE, &id, &password) })
                .await;
            if res.is_err() {
                this.update(cx, |this, cx| {
                    this.push_toast(
                        "Couldn't save password to keychain",
                        ToastVariant::Error,
                        cx,
                    );
                })
                .ok();
            }
        })
        .detach();
    }

    /// Send a throwaway `TestConnection` probe for the editor's current form. On
    /// edit with a blank password field, the stored secret is fetched first.
    pub fn test_editor_connection(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let name = editor.name.read(cx).content().trim().to_string();
        let host = editor.host.read(cx).content().trim().to_string();
        let port_text = editor.port.read(cx).content().trim().to_string();
        let username = editor.username.read(cx).content().trim().to_string();
        let remote_path = editor.remote_path.read(cx).content().trim().to_string();
        let password = editor.password.read(cx).content().to_string();
        let protocol = editor.protocol;
        let id = editor.id.clone();
        let is_new = editor.is_new;

        if host.is_empty() || username.is_empty() {
            self.set_test_status(false, "Host and username are required");
            return;
        }
        let port = if port_text.is_empty() {
            protocol.default_port()
        } else {
            match port_text.parse::<u16>() {
                Ok(port) => port,
                Err(_) => {
                    self.set_test_status(false, "Port must be a number");
                    return;
                }
            }
        };
        let profile = Profile {
            id: id.clone(),
            name: if name.is_empty() { "test".into() } else { name },
            protocol,
            host,
            port,
            username,
            remote_path: (!remote_path.is_empty()).then_some(remote_path),
            color: ProfileColor::default(),
            last_connected: None,
        };
        if let Some(editor) = self.editor.as_mut() {
            editor.testing = true;
            editor.test_status = None;
        }

        if password.is_empty() && !is_new {
            // Editing with a blank field — probe with the stored secret.
            let keyring = self.keyring;
            let lookup_id = id;
            cx.spawn(async move |this, cx| {
                let res = cx
                    .background_executor()
                    .spawn(async move { keyring.get_password(KEYCHAIN_SERVICE, &lookup_id) })
                    .await;
                this.update(cx, |this, cx| {
                    let pw = res.ok().flatten().unwrap_or_default();
                    this.dispatch_test(profile, pw);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        } else {
            self.dispatch_test(profile, password);
        }
        cx.notify();
    }

    /// Set the editor's inline test status (if the editor is still open).
    fn set_test_status(&mut self, ok: bool, message: impl Into<SharedString>) {
        if let Some(editor) = self.editor.as_mut() {
            editor.testing = false;
            editor.test_status = Some(TestStatus {
                ok,
                message: message.into(),
            });
        }
    }

    /// Send the `TestConnection` command, reflecting a send failure inline.
    fn dispatch_test(&mut self, profile: Profile, password: String) {
        let sent = self.service.send(Command::TestConnection {
            profile,
            password: Secret::new(password),
        });
        if !sent {
            self.set_test_status(false, "Backend unavailable");
        }
    }

    /// Open the sidebar row context menu (Edit / Remove) at a cursor position.
    pub fn open_row_menu(
        &mut self,
        profile_id: String,
        profile_name: SharedString,
        position: Point<Pixels>,
    ) {
        self.row_menu = Some(RowMenu {
            profile_id,
            profile_name,
            position,
        });
    }

    /// Dismiss the row context menu.
    pub fn close_row_menu(&mut self) {
        self.row_menu = None;
    }

    /// Open the "remove connection?" confirmation for a profile.
    pub fn open_delete_confirm(&mut self, profile_id: String, profile_name: SharedString) {
        self.row_menu = None;
        self.delete_confirm = Some(DeleteConfirm {
            profile_id,
            profile_name,
        });
    }

    /// Dismiss the delete confirmation without deleting.
    pub fn cancel_delete(&mut self) {
        self.delete_confirm = None;
    }

    /// Delete the confirmed profile from the store and its keychain entry.
    pub fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.delete_confirm.take() else {
            return;
        };
        let id = confirm.profile_id;
        if let Err(err) = self.store.delete(&id) {
            self.push_toast(err.to_string(), ToastVariant::Error, cx);
            return;
        }
        // Best-effort, idempotent keychain cleanup off-thread.
        let keyring = self.keyring;
        let id_for_keyring = id.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = keyring.delete_password(KEYCHAIN_SERVICE, &id_for_keyring);
            })
            .detach();
        if self.editor.as_ref().is_some_and(|e| e.id == id) {
            self.editor = None;
        }
        self.reload_connections(cx);
        self.push_toast(
            format!("Removed “{}”", confirm.profile_name),
            ToastVariant::Success,
            cx,
        );
    }

    // --- browser file operations (M4) ------------------------------------

    /// The absolute remote path of an entry `name` in the current directory.
    fn remote_path_of(&self, name: &str) -> String {
        let cwd = self.current_path();
        if cwd == "/" {
            format!("/{name}")
        } else {
            format!("{cwd}/{name}")
        }
    }

    /// Open the file-row context menu, applying file-manager selection semantics:
    /// a right-click on an unselected row replaces the selection with just it; a
    /// right-click inside the selection keeps the multi-selection (plan D7).
    pub fn open_file_menu(&mut self, name: SharedString, is_dir: bool, position: Point<Pixels>) {
        if !self.selected.contains(&name) {
            self.selected.clear();
            self.selected.insert(name.clone());
        }
        self.file_menu = Some(FileMenu {
            name,
            is_dir,
            position,
        });
    }

    /// Dismiss the file-row context menu.
    pub fn close_file_menu(&mut self) {
        self.file_menu = None;
    }

    /// Open the **New folder** input modal (blank, "Create").
    pub fn start_new_folder(&mut self, cx: &mut Context<Self>) {
        self.close_file_menu();
        let input = cx.new(|cx| TextInput::new(cx).with_placeholder("Folder name"));
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        self.wire_input(&input, cx);
        self.input_prompt = Some(InputPrompt {
            title: "New folder".into(),
            label: "Name".into(),
            submit_label: "Create".into(),
            input,
            action: InputAction::NewFolder,
        });
    }

    /// Open the **Rename** input modal, prefilled with the clicked row's name.
    pub fn start_rename(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.file_menu.as_ref() else {
            return;
        };
        let name = menu.name.clone();
        self.close_file_menu();
        self.open_rename_prompt(name, cx);
    }

    /// Open the **Rename** modal for the current single-row selection — the
    /// keyboard (F2) entry point that has no context menu to read (plan M6 D11).
    pub fn rename_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.len() != 1 {
            return;
        }
        let Some(name) = self.selected.iter().next().cloned() else {
            return;
        };
        self.open_rename_prompt(name, cx);
    }

    /// Build and show the rename modal for `name` (shared by the menu + F2).
    fn open_rename_prompt(&mut self, name: SharedString, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new(cx).with_content(name.clone()));
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        self.wire_input(&input, cx);
        self.input_prompt = Some(InputPrompt {
            title: "Rename".into(),
            label: "New name".into(),
            submit_label: "Rename".into(),
            input,
            action: InputAction::Rename { original: name },
        });
    }

    /// Activate the current selection (the browser's Enter key, plan M6 D11): a
    /// single selected directory is opened; otherwise the selection is downloaded
    /// (files only — folders are skipped with a toast by `download_selection`).
    pub fn activate_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.len() == 1 {
            if let Some(name) = self.selected.iter().next().cloned() {
                let is_dir = self
                    .listing
                    .iter()
                    .find(|row| row.entry.name.as_str() == name.as_ref())
                    .is_some_and(|row| row.entry.is_dir);
                if is_dir {
                    self.open_dir(&name, cx);
                    return;
                }
            }
        }
        self.download_selection(cx);
    }

    /// Dismiss the input modal without acting.
    pub fn cancel_input(&mut self) {
        self.input_prompt = None;
    }

    /// Validate and submit the input modal → `Mkdir` / `Rename`. Rejects an empty
    /// name or one containing `/`; an unchanged rename is a no-op (plan D8).
    pub fn submit_input(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.input_prompt.as_ref() else {
            return;
        };
        let value = prompt.input.read(cx).content().trim().to_string();
        if value.is_empty() {
            self.push_toast("Name can't be empty", ToastVariant::Error, cx);
            return;
        }
        if value.contains('/') {
            self.push_toast("Name can't contain a slash", ToastVariant::Error, cx);
            return;
        }
        let action = prompt.action.clone();
        self.input_prompt = None;
        let command = match action {
            InputAction::NewFolder => Command::Mkdir {
                path: self.remote_path_of(&value),
            },
            InputAction::Rename { original } => {
                if value == original.as_ref() {
                    return; // unchanged → nothing to do
                }
                Command::Rename {
                    from: self.remote_path_of(&original),
                    to: self.remote_path_of(&value),
                }
            }
        };
        if !self.service.send(command) {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Open the file-delete confirmation for the current selection.
    pub fn start_delete(&mut self, _cx: &mut Context<Self>) {
        self.close_file_menu();
        // Snapshot each selected entry with its is_dir flag (for recursive delete).
        let entries: Vec<(SharedString, bool)> = self
            .selected
            .iter()
            .filter_map(|name| {
                self.listing
                    .iter()
                    .find(|row| row.entry.name.as_str() == name.as_ref())
                    .map(|row| (name.clone(), row.entry.is_dir))
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        self.file_delete = Some(FileDeleteConfirm { entries });
    }

    /// Dismiss the file-delete confirmation without deleting.
    pub fn cancel_file_delete(&mut self) {
        self.file_delete = None;
    }

    /// Issue one `Remove` per confirmed entry (file or recursive folder, D8).
    pub fn confirm_file_delete(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.file_delete.take() else {
            return;
        };
        let mut ok = true;
        for (name, is_dir) in &confirm.entries {
            let path = self.remote_path_of(name);
            if !self.service.send(Command::Remove {
                path,
                is_dir: *is_dir,
            }) {
                ok = false;
            }
        }
        if !ok {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Copy the clicked entry's absolute remote path to the clipboard (plan D10:
    /// no service round-trip).
    pub fn copy_path(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.file_menu.take() else {
            return;
        };
        let path = self.remote_path_of(&menu.name);
        cx.write_to_clipboard(ClipboardItem::new_string(path));
        self.push_toast("Path copied", ToastVariant::Success, cx);
    }

    /// Download the current selection. A single file opens a save-as dialog;
    /// several files open a folder picker (one `Download` per file). Folders in
    /// the selection are skipped with a toast (directory download is post-MVP, D5).
    pub fn download_selection(&mut self, cx: &mut Context<Self>) {
        self.close_file_menu();
        let mut files: Vec<(String, String)> = Vec::new();
        let mut skipped_folder = false;
        for name in &self.selected {
            let Some(row) = self
                .listing
                .iter()
                .find(|row| row.entry.name.as_str() == name.as_ref())
            else {
                continue;
            };
            if row.entry.is_dir {
                skipped_folder = true;
                continue;
            }
            files.push((self.remote_path_of(name), name.to_string()));
        }
        if skipped_folder {
            self.push_toast("Folders can't be downloaded yet", ToastVariant::Info, cx);
        }
        if files.is_empty() {
            return;
        }
        let dir = default_download_dir();

        if files.len() == 1 {
            let (remote, name) = files.into_iter().next().expect("one file");
            let receiver = cx.prompt_for_new_path(&dir, Some(&name));
            cx.spawn(async move |this, cx| {
                if let Ok(Ok(Some(local))) = receiver.await {
                    this.update(cx, |this, cx| {
                        if !this.service.send(Command::Download { remote, local }) {
                            this.push_toast("Backend unavailable", ToastVariant::Error, cx);
                        }
                        cx.notify();
                    })
                    .ok();
                }
            })
            .detach();
        } else {
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
                    for (remote, name) in files {
                        let local = folder.join(&name);
                        if !this.service.send(Command::Download { remote, local }) {
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
    }

    /// Upload one or more chosen local files into the current directory (plan D5).
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

    /// Upload already-known local files (from a drag-and-drop, plan post-MVP
    /// drag-in). `subdir`, when set, is a directory *in the current folder* the
    /// files were dropped onto; otherwise they land in the current folder.
    /// Folders are skipped (recursive upload is post-MVP) with a toast.
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
        let mut skipped_dir = false;
        for local in paths {
            if local.is_dir() {
                skipped_dir = true;
                continue;
            }
            let Some(name) = local.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let remote = match subdir.as_ref() {
                Some(dir) => format!("{}/{}", self.remote_path_of(dir), name),
                None => self.remote_path_of(name),
            };
            if self.service.send(Command::Upload { local, remote }) {
                sent += 1;
            } else {
                ok = false;
            }
        }
        if skipped_dir {
            self.push_toast("Folders can't be uploaded yet", ToastVariant::Info, cx);
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

    /// Cancel a queued or running transfer (the dock's `x` button, M5 D7). The
    /// row updates reactively when the matching [`Event::TransferDone`] arrives.
    pub fn cancel_transfer(&mut self, id: TransferId) {
        self.service.send(Command::CancelTransfer { id });
    }

    /// Re-issue a failed transfer (the dock's retry button). Resends the original
    /// `Upload`/`Download` command and drops the stale failed row — the retry
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
        let command = match vm.transfer.direction {
            TransferDirection::Upload => Command::Upload { local, remote },
            TransferDirection::Download => Command::Download { remote, local },
        };
        if self.service.send(command) {
            self.transfers.retain(|t| t.transfer.id != id);
        } else {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
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
                self.used_stored_password = None;
                // Stamp the successful connect and persist it, so "Recent"
                // ordering survives a restart (plan M3 D6).
                self.stamp_last_connected(&profile_id, cx);
                self.enter_browser(profile_id, cx);
            }
            Event::DirListing { path, entries } => {
                // Drop a listing for a directory we've since navigated away from.
                if path == self.current_path() {
                    self.listing = entries.into_iter().map(EntryRow::new).collect();
                    self.listing_loading = false;
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
                // Always toast the success line; refresh the listing only for the
                // mutating ops. Transfers (Download/Upload) no longer arrive here
                // — they feed the dock and refresh via `TransferDone` (M5 D5/D10).
                self.push_toast(message, ToastVariant::Success, cx);
                if !matches!(op, FileOp::Download) {
                    self.selected.clear();
                    self.reload_listing(cx);
                }
            }
            // A new transfer entered the queue: add a Queued dock row (M5 D10).
            Event::TransferQueued {
                id,
                direction,
                remote,
                local,
            } => {
                self.transfers.push(TransferVm {
                    transfer: Transfer {
                        id,
                        direction,
                        remote_path: remote,
                        local_path: local,
                        total_bytes: None,
                        transferred_bytes: 0,
                        status: TransferStatus::Queued,
                    },
                    speed_bps: None,
                    error: None,
                });
            }
            // A transfer left the queue and is running: set Running + total.
            Event::TransferStarted { id, total } => {
                if let Some(vm) = self.transfers.iter_mut().find(|t| t.transfer.id == id) {
                    vm.transfer.status = TransferStatus::Running;
                    vm.transfer.total_bytes = total;
                }
            }
            // A throttled progress sample. Ignore it for a row that is no longer
            // Running (a late tick can arrive after TransferDone, M5 D10/Risks).
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
            // A transfer reached a terminal state. Keep the row so the
            // Completed/Failed tabs populate; on a completed upload into the
            // current directory, refresh the listing (the refresh that used to
            // live on the FileOpDone path, M5 D5/D10).
            Event::TransferDone {
                id,
                status,
                message,
            } => {
                let cwd = self.current_path();
                let mut refresh = false;
                if let Some(vm) = self.transfers.iter_mut().find(|t| t.transfer.id == id) {
                    vm.transfer.status = status;
                    vm.speed_bps = None;
                    match status {
                        TransferStatus::Completed => {
                            // Snap the bar to 100% even if no final sample landed.
                            if let Some(total) = vm.transfer.total_bytes {
                                vm.transfer.transferred_bytes = total;
                            }
                            refresh = vm.transfer.direction == TransferDirection::Upload
                                && remote_parent(&vm.transfer.remote_path) == cwd;
                        }
                        TransferStatus::Failed => {
                            vm.error = message.map(SharedString::from);
                        }
                        _ => {}
                    }
                }
                if refresh {
                    self.reload_listing(cx);
                }
            }
            Event::Error { message } => {
                let stale = self.used_stored_password.take();
                self.host_key_prompt = None;
                self.connecting_id = None;
                self.listing_loading = false;
                self.push_toast(message.clone(), ToastVariant::Error, cx);
                // A stored password that fails auth is likely stale — re-open the
                // prompt so the user can correct (and overwrite) it (plan D5.3).
                if message.contains("authentication failed") {
                    if let Some(id) = stale {
                        if let Some(conn) = self.connections.iter().find(|c| c.profile.id == id) {
                            let profile = conn.profile.clone();
                            self.show_password_prompt(profile, cx);
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
            // Lifecycle pings (and any future variants) need no UI change.
            Event::Stopped => {}
            _ => {}
        }
        cx.notify();
    }

    /// Stamp a profile's `last_connected` to now and persist it, then refresh the
    /// in-memory connection list (for the "Recent" labels/ordering).
    fn stamp_last_connected(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        if let Ok(Some(mut profile)) = self.store.get(profile_id) {
            profile.last_connected = Some(OffsetDateTime::now_utc());
            if let Err(err) = self.store.save(&profile) {
                self.push_toast(err.to_string(), ToastVariant::Error, cx);
                return;
            }
            self.reload_connections(cx);
        }
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
        // The dock is fed by real transfer events (M5); it starts empty.
        self.transfers = Vec::new();
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

/// The default local directory for a download save-as: the OS Downloads folder,
/// falling back to the home directory, then the current directory.
fn default_download_dir() -> std::path::PathBuf {
    let dirs = directories::UserDirs::new();
    dirs.as_ref()
        .and_then(|u| u.download_dir().map(|p| p.to_path_buf()))
        .or_else(|| dirs.as_ref().map(|u| u.home_dir().to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Split a remote path into non-empty segments.
fn path_segments(path: &str) -> Vec<SharedString> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(SharedString::from)
        .collect()
}

/// The parent directory of an absolute remote file path (`"/"` at the root),
/// matching [`AppState::current_path`]'s form so the two can be compared to
/// decide whether a completed upload landed in the current directory.
fn remote_parent(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((parent, _)) if !parent.is_empty() => parent.to_string(),
        _ => "/".to_string(),
    }
}
