//! [`AppState`] — the single source of truth for the app shell.
//!
//! One root `Entity<AppState>` holds all mutable state plus the interaction
//! logic (navigation, sort, filter, selection, dock). Views are `RenderOnce`
//! helpers that read a `&AppState` and emit elements; only the filter
//! [`TextInput`] is its own entity (it needs focus/IME state).

pub mod models;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::channel::oneshot;
use futures::StreamExt;
use gpui::{
    point, prelude::*, px, App, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    PathPromptOptions, Pixels, Point, SharedString, Window,
};
use nyx_core::{
    CollisionChoice, EntryKind, EntryOutcomeKind, FtpsMode, Protocol, RemotePath, Secret,
    ServerTrustKind, Transfer, TransferDirection, TransferId, TransferKind, TransferStatus,
};
use nyx_drag::DragFile;
use nyx_keyring::{passphrase_account, password_account, CredentialStore, OsKeyring};
use nyx_profile::{
    AuthMethod, FileProfileStore, FileSettingsStore, Profile, ProfileColor, ProfileStore, Settings,
};
use nyx_service::{Command, Event, FileOp, ServiceHandle};
use nyx_ui::{ActiveTheme, TextInput, TextInputEvent, Theme, ToastVariant};
use time::OffsetDateTime;

use models::{AccentKind, ConnectionVm, Density, DockTab, EntryRow, SortKey, TransferVm};

/// Visible folder rows' painted rects (name → window-coordinate rect), shared
/// between the file-table paint that fills it and the drag-return hit test.
type DropRowBounds = Rc<RefCell<Vec<(SharedString, Bounds<Pixels>)>>>;

use crate::drag::{DragDownloads, ServiceDragFetch};

/// The keychain service name. Secrets are addressed `("nyx", account)`, where the
/// account is derived per-profile by [`password_account`] / [`passphrase_account`].
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

/// The secret prompt shown on a keychain miss or a locked key. The secret is read
/// straight from the masked input into the `Connect` command, never stored on
/// `AppState`. It prompts for a password or — when [`is_passphrase`] — a key
/// passphrase.
///
/// [`is_passphrase`]: PasswordPrompt::is_passphrase
pub struct PasswordPrompt {
    /// The profile being connected to.
    pub profile_id: String,
    /// Display name (modal title).
    pub profile_name: SharedString,
    /// `user@host:port` shown under the title.
    pub host_label: SharedString,
    /// The masked secret field.
    pub input: Entity<TextInput>,
    /// Whether to write the entered secret to the keychain on connect.
    pub save_to_keychain: bool,
    /// Whether this prompts for a key passphrase (vs a login password).
    pub is_passphrase: bool,
}

/// A pending trust-on-first-use prompt (an unknown host key or TLS certificate
/// was presented).
pub struct HostKeyPrompt {
    /// The host the identity belongs to.
    pub host: SharedString,
    /// The SHA-256 fingerprint, e.g. `SHA256:…`.
    pub fingerprint: SharedString,
    /// Whether this is an SSH host key (SFTP) or a TLS certificate (FTPS).
    pub kind: ServerTrustKind,
}

/// One transfer parked at the pre-flight gate because its destination exists.
/// Several can stack up (a multi-file batch); the modal shows them one at a time.
pub struct CollisionInfo {
    /// The parked transfer's id.
    pub id: TransferId,
    /// Upload or download (which side the existing destination is on).
    pub direction: TransferDirection,
    /// Whether the existing destination is a directory (folder merge prompt).
    pub is_dir: bool,
    /// The destination's final name (for the prompt copy).
    pub name: SharedString,
    /// The full destination path (remote for upload, local for download).
    pub path: SharedString,
    /// Size of the existing destination, if known.
    pub existing_size: Option<u64>,
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
    /// Whether key auth is selected (vs password). Mutually exclusive with
    /// `auth_is_anonymous`.
    pub auth_is_key: bool,
    /// Whether anonymous auth is selected (FTP/FTPS only). Mutually exclusive with
    /// `auth_is_key`.
    pub auth_is_anonymous: bool,
    /// Private-key file path field (key auth).
    pub key_path: Entity<TextInput>,
    /// Key passphrase field (obscured, optional). Blank = unencrypted key, or on
    /// edit = "keep the stored passphrase".
    pub passphrase: Entity<TextInput>,
    /// Selected protocol.
    pub protocol: Protocol,
    /// Selected FTPS TLS mode (only meaningful when `protocol` is FTPS).
    pub ftps_mode: FtpsMode,
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

/// A pending right-click context menu on a browser file row. Delete/Download
/// operate on the whole selection; Rename / Copy path target the clicked row.
pub struct FileMenu {
    /// The clicked row's name (the single-target ops act on this).
    pub name: SharedString,
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

    /// The current working directory (canonical, absolute).
    pub cwd: RemotePath,
    /// Back/forward navigation stack.
    pub history: Vec<RemotePath>,
    /// Cursor into `history`.
    pub history_ix: usize,
    /// Listing for the current `cwd`. Behind an `Rc` so the browser view can
    /// hand it to its `'static` row closures without cloning the entries.
    pub listing: Rc<Vec<EntryRow>>,
    /// Indices into [`listing`](Self::listing) giving the visible order (filtered
    /// by [`filter_lower`](Self::filter_lower), then sorted, folders first).
    /// Rebuilt only when the listing, sort, or filter changes — never per frame.
    view_order: Rc<Vec<usize>>,
    /// The stateful filter box.
    pub filter: Entity<TextInput>,
    /// Lower-cased, trimmed filter text, kept in sync with [`filter`](Self::filter)
    /// so [`rebuild_view_order`](Self::rebuild_view_order) needs no `cx`.
    filter_lower: String,
    /// Active sort: `(key, ascending)`.
    pub sort: (SortKey, bool),
    /// Selected entry names.
    pub selected: HashSet<SharedString>,
    /// The selection anchor: the row a range-select (shift-click) extends from.
    /// Set by every plain / additive click; consulted by [`select_range`].
    ///
    /// [`select_range`]: AppState::select_range
    select_anchor: Option<SharedString>,

    /// Whether the dock body is expanded.
    pub dock_open: bool,
    /// Active dock filter tab.
    pub dock_tab: DockTab,
    /// All transfers.
    pub transfers: Vec<TransferVm>,

    /// Whether the sidebar is shown.
    pub sidebar_open: bool,
    /// Whether the sidebar's **Recent** group is collapsed (session-only).
    pub recent_collapsed: bool,
    /// Focus target for the file browser's `"Browser"` key context
    /// (Enter / Backspace / F2 / Delete).
    pub browser_focus: FocusHandle,
    /// Always-focusable root. Keeps the `"App"` key context in the dispatch path
    /// so global shortcuts and modal Enter/Esc fire even when nothing else holds
    /// focus (GPUI only dispatches keys along the focused element's ancestry).
    pub root_focus: FocusHandle,
    /// A focus target to apply on the next render — modal autofocus, focusing the
    /// file table on connect, etc. Consumed once.
    pending_focus: Option<FocusHandle>,
    /// Handle for the open modal's primary button. Field-less modals autofocus it
    /// so Enter activates the default action (GPUI fires the focused button's
    /// click natively — no separate confirm action that would double-fire).
    pub modal_primary_focus: FocusHandle,
    /// Stable per-row focus handles for Tab navigation of the welcome connection
    /// list, keyed `"card:<id>"`, `"recent:<id>"`, and `"new"`.
    row_focus: HashMap<String, FocusHandle>,
    /// Whether the tweaks modal is open.
    pub tweaks_open: bool,
    /// Whether the keyboard-shortcuts cheat-sheet is open.
    pub shortcuts_open: bool,
    /// Whether the color-scheme dropdown inside the tweaks modal is open.
    pub theme_select_open: bool,
    /// File-row density (exercises `Table::row_height`).
    pub density: Density,
    /// Whether the permissions column is shown.
    pub show_perms: bool,
    /// The current toast, if any.
    pub toast: Option<ToastMsg>,
    /// Monotonic toast id source.
    next_toast_id: u64,

    /// On-disk profile store (the source of `connections`).
    store: FileProfileStore,
    /// On-disk store for UI preferences (theme, density, permissions column).
    settings_store: FileSettingsStore,
    /// OS keychain for connection passwords (addressed by profile id).
    keyring: OsKeyring,
    /// A startup error to surface once the backend is `Ready` (e.g. a malformed
    /// `profiles.toml`); kept because construction can't push a toast.
    startup_error: Option<SharedString>,

    /// Handle to the backend thread (dropped on app exit → graceful shutdown).
    service: ServiceHandle,
    /// Correlates drag-out promise downloads with their transfer events. Fed by
    /// the event loop; awaited off-thread by the drag-promise callback.
    drag_downloads: DragDownloads,
    /// Painted bounds of the visible folder rows (name → on-screen rect), in GPUI
    /// window coordinates. Refreshed each render of the file table and consulted
    /// when an OS drag-out returns inside the window, to find the folder under the
    /// drop point. See [`AppState::handoff_drag_out`].
    drop_row_bounds: DropRowBounds,
    /// While an OS drag-out is back inside the window, the folder row currently
    /// under the cursor — highlighted so the (unchangeable native) cursor still
    /// has a visible drop target. `None` when outside or over a non-folder.
    pub drag_return_folder: Option<SharedString>,
    /// The profile id of an in-flight connection attempt, if any.
    pub connecting_id: Option<String>,
    /// The profile id whose connect used a *stored* password — set so an auth
    /// failure can re-open the prompt to correct a stale keychain entry.
    used_stored_password: Option<String>,
    /// A pending password prompt (shown before connecting).
    pub password_prompt: Option<PasswordPrompt>,
    /// A pending host-key trust prompt (unknown key).
    pub host_key_prompt: Option<HostKeyPrompt>,
    /// Transfers parked at the pre-flight gate, awaiting an overwrite decision.
    /// The modal resolves the front one; the rest follow.
    pub pending_collisions: Vec<CollisionInfo>,
    /// The "apply to all" toggle's state in the collision modal.
    pub collision_apply_all: bool,
    /// The connection editor modal, if open.
    pub editor: Option<ConnectionEditor>,
    /// A pending sidebar row context menu, if open.
    pub row_menu: Option<RowMenu>,
    /// A pending delete confirmation, if open.
    pub delete_confirm: Option<DeleteConfirm>,
    /// A pending browser file-row context menu, if open.
    pub file_menu: Option<FileMenu>,
    /// A pending New-folder / Rename input modal, if open.
    pub input_prompt: Option<InputPrompt>,
    /// A pending file-delete confirmation, if open.
    pub file_delete: Option<FileDeleteConfirm>,
    /// Whether a directory listing is in flight (drives the loading hint).
    pub listing_loading: bool,
    /// Set when the active connection's transport dropped: a credential-free
    /// reason that drives the non-modal "Connection lost — Reconnect" banner.
    /// `None` while connected. The last listing stays visible underneath.
    pub connection_lost: Option<SharedString>,
    /// While auto-reconnect is running after a loss: the current attempt number,
    /// for the banner copy. `None` when not actively auto-reconnecting.
    pub reconnect_attempt: Option<u32>,
    /// Set when auto-reconnect gave up: the banner says "Reconnect failed" rather
    /// than the plain "Connection lost". Both still offer a manual reconnect.
    pub reconnect_failed: bool,
    /// Whether a dropped session should auto-reconnect (persisted in `Settings`).
    pub auto_reconnect: bool,
}

/// Map a persisted theme name to its concrete [`Theme`], defaulting to One Dark
/// for an unknown name (e.g. a theme that was renamed or removed).
pub fn theme_from_name(name: &str) -> Theme {
    match name {
        "GitHub Dark" => Theme::github_dark(),
        "Ayu Dark" => Theme::ayu_dark(),
        _ => Theme::one_dark(),
    }
}

impl AppState {
    /// Build the initial state: welcome screen, connections loaded, nothing open.
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Not in the Tab ring — reachable via cmd-f, and keeping it out lets a
        // modal trap focus among its own fields/buttons.
        let filter = cx.new(|cx| {
            TextInput::new(cx)
                .with_placeholder("Filter this folder…")
                .tab_stop(false)
        });
        cx.observe(&filter, |this, _, cx| {
            this.refilter(cx);
            cx.notify();
        })
        .detach();
        // Esc/Enter in the filter hand focus back to the file table (it's out of
        // the Tab ring, so this is the only keyboard way out). The filter text is
        // left intact — Esc exits the field, it doesn't clear the filter.
        cx.subscribe(&filter, |this, _input, _event: &TextInputEvent, cx| {
            this.arm_focus(this.browser_focus.clone());
            cx.notify();
        })
        .detach();
        let browser_focus = cx.focus_handle();
        let root_focus = cx.focus_handle();
        let modal_primary_focus = cx.focus_handle().tab_stop(true);

        // Spawn the backend thread and drain its events into this entity. The
        // drain runs on the GPUI foreground executor: `next().await` yields, so it
        // never blocks the UI. This is the single Tokio↔GPUI bridge.
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

        // Missing file → empty list (first run); malformed → surfaced as a toast
        // once `Ready`, and the store is not overwritten so the user can fix it.
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

        // A missing/malformed settings file is silently the default.
        let settings_store = FileSettingsStore::open_default()
            .unwrap_or_else(|_| FileSettingsStore::with_path("settings.toml"));
        let settings = settings_store.load();
        cx.set_global(theme_from_name(&settings.theme));
        let density = Density::ALL[(settings.density as usize).min(Density::ALL.len() - 1)];
        let show_perms = settings.show_perms;
        let auto_reconnect = settings.auto_reconnect;

        let mut row_focus = HashMap::new();
        row_focus.insert("new".to_string(), cx.focus_handle().tab_stop(true));
        for conn in &connections {
            let id = &conn.profile.id;
            row_focus.insert(format!("card:{id}"), cx.focus_handle().tab_stop(true));
            row_focus.insert(format!("recent:{id}"), cx.focus_handle().tab_stop(true));
        }

        Self {
            view: View::Welcome,
            connections,
            active_id: None,
            online_id: None,
            cwd: RemotePath::root(),
            history: vec![RemotePath::root()],
            history_ix: 0,
            listing: Rc::new(Vec::new()),
            view_order: Rc::new(Vec::new()),
            filter,
            filter_lower: String::new(),
            sort: (SortKey::Name, true),
            selected: HashSet::new(),
            select_anchor: None,
            dock_open: true,
            dock_tab: DockTab::All,
            transfers: Vec::new(),
            sidebar_open: true,
            recent_collapsed: false,
            browser_focus,
            root_focus,
            pending_focus: None,
            modal_primary_focus,
            row_focus,
            tweaks_open: false,
            shortcuts_open: false,
            theme_select_open: false,
            density,
            show_perms,
            toast: None,
            next_toast_id: 0,
            store,
            keyring: OsKeyring::new(),
            settings_store,
            startup_error,
            service,
            drag_downloads: DragDownloads::new(),
            drop_row_bounds: Rc::new(RefCell::new(Vec::new())),
            drag_return_folder: None,
            connecting_id: None,
            used_stored_password: None,
            password_prompt: None,
            host_key_prompt: None,
            pending_collisions: Vec::new(),
            collision_apply_all: false,
            editor: None,
            row_menu: None,
            delete_confirm: None,
            file_menu: None,
            input_prompt: None,
            file_delete: None,
            listing_loading: false,
            connection_lost: None,
            reconnect_attempt: None,
            reconnect_failed: false,
            auto_reconnect,
        }
    }

    /// Persist the current UI preferences to disk. Best-effort: a write failure
    /// is logged, not surfaced.
    pub fn save_settings(&self, cx: &App) {
        let settings = Settings {
            theme: cx.theme().name.to_string(),
            density: self.density.index() as u8,
            show_perms: self.show_perms,
            auto_reconnect: self.auto_reconnect,
        };
        if let Err(err) = self.settings_store.save(&settings) {
            tracing::warn!("failed to persist settings: {err}");
        }
    }

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
                self.sync_row_focus(cx);
            }
            Err(err) => self.push_toast(err.to_string(), ToastVariant::Error, cx),
        }
    }

    /// Ensure every connection row (and the New button) has a stable Tab-stop
    /// focus handle. Idempotent; called whenever the connection list changes.
    fn sync_row_focus(&mut self, cx: &mut Context<Self>) {
        self.row_focus
            .entry("new".to_string())
            .or_insert_with(|| cx.focus_handle().tab_stop(true));
        let ids: Vec<String> = self
            .connections
            .iter()
            .map(|c| c.profile.id.clone())
            .collect();
        for id in ids {
            self.row_focus
                .entry(format!("card:{id}"))
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
            self.row_focus
                .entry(format!("recent:{id}"))
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
    }

    /// A stable focus handle for a welcome-list row, if one exists.
    pub fn row_focus(&self, key: &str) -> Option<FocusHandle> {
        self.row_focus.get(key).cloned()
    }

    /// Take the focus target queued for this render (modal autofocus, etc.).
    pub fn take_pending_focus(&mut self) -> Option<FocusHandle> {
        self.pending_focus.take()
    }

    /// The element that should hold focus when nothing else does: the file table
    /// while browsing (so arrow keys work), otherwise the root.
    pub fn default_focus(&self) -> FocusHandle {
        if self.view == View::Browse && !self.has_overlay() {
            self.browser_focus.clone()
        } else {
            self.root_focus.clone()
        }
    }

    /// Queue `handle` to receive focus on the next render.
    fn arm_focus(&mut self, handle: FocusHandle) {
        self.pending_focus = Some(handle);
    }

    /// Queue the root for focus on the next render. Used for overlays with no
    /// primary button (menus, the cheat-sheet) so Esc still routes via `"App"`.
    fn arm_root_focus(&mut self) {
        self.pending_focus = Some(self.root_focus.clone());
    }

    /// Queue the open modal's primary button for focus, so it shows the focus ring
    /// and Enter/Space activate it. Used for field-less confirmation modals.
    fn arm_primary_focus(&mut self) {
        self.pending_focus = Some(self.modal_primary_focus.clone());
    }

    /// Queue a text field to receive focus on the next render (modal autofocus).
    fn arm_input_focus(&mut self, input: &Entity<TextInput>, cx: &App) {
        self.pending_focus = Some(input.read(cx).focus_handle(cx));
    }

    /// Queue the editor's name field for focus on the next render.
    fn arm_editor_focus(&mut self, cx: &App) {
        if let Some(name) = self.editor.as_ref().map(|e| e.name.clone()) {
            self.arm_input_focus(&name, cx);
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
    /// `connecting_id` is set up-front so the UI shows progress while the
    /// (potentially dialog-popping) keychain lookup runs off the GPUI thread.
    pub fn open_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.connections.iter().find(|c| c.profile.id == id) else {
            return;
        };
        let profile = conn.profile.clone();
        self.connecting_id = Some(id.to_string());

        // Anonymous login carries no secret: skip the keychain entirely.
        if matches!(profile.auth, AuthMethod::Anonymous) {
            self.send_connect(profile, String::new(), false, cx);
            return;
        }

        let keyring = self.keyring;
        let account = secret_account(&profile);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { keyring.get_password(KEYCHAIN_SERVICE, &account) })
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
            Ok(Some(secret)) => self.send_connect(profile, secret, true, cx),
            // No stored secret. For password auth, prompt. For key auth, try with
            // no passphrase (an unencrypted key needs none) — an encrypted key
            // comes back as `KeyLocked` and we prompt for the passphrase then.
            Ok(None) => match profile.auth {
                AuthMethod::Password => self.show_password_prompt(profile, cx),
                AuthMethod::Key { .. } | AuthMethod::Anonymous => {
                    self.send_connect(profile, String::new(), false, cx)
                }
            },
            Err(err) => {
                self.connecting_id = None;
                self.push_toast(format!("Keychain error: {err}"), ToastVariant::Error, cx);
            }
        }
    }

    /// Show the password prompt for `profile` (a keychain miss, or a stale-secret
    /// re-prompt).
    fn show_password_prompt(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.show_secret_prompt(profile, false, cx);
    }

    /// Show the passphrase prompt for `profile` (a locked key, or a stale-secret
    /// re-prompt).
    fn show_passphrase_prompt(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.show_secret_prompt(profile, true, cx);
    }

    /// Build and show the secret prompt — a password or, when `is_passphrase`, a
    /// key passphrase. The "Save to keychain" toggle defaults on.
    fn show_secret_prompt(
        &mut self,
        profile: Profile,
        is_passphrase: bool,
        cx: &mut Context<Self>,
    ) {
        let host_label = format!("{}@{}:{}", profile.username, profile.host, profile.port);
        let placeholder = if is_passphrase {
            "Passphrase"
        } else {
            "Password"
        };
        let input = cx.new(|cx| TextInput::new(cx).with_placeholder(placeholder).obscured());
        self.wire_input(&input, cx);
        self.arm_input_focus(&input, cx);
        self.password_prompt = Some(PasswordPrompt {
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone().into(),
            host_label: host_label.into(),
            input,
            save_to_keychain: true,
            is_passphrase,
        });
    }

    /// Send a `Connect` command, tracking whether a *stored* secret was used.
    fn send_connect(
        &mut self,
        profile: Profile,
        secret: String,
        from_keychain: bool,
        cx: &mut Context<Self>,
    ) {
        let id = profile.id.clone();
        self.connecting_id = Some(id.clone());
        self.used_stored_password = from_keychain.then_some(id);
        let sent = self.service.send(Command::Connect {
            profile,
            secret: Secret::new(secret),
            auto_reconnect: self.auto_reconnect,
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
        let secret = prompt.input.read(cx).content().to_string();
        let Some(conn) = self
            .connections
            .iter()
            .find(|c| c.profile.id == prompt.profile_id)
        else {
            return;
        };
        let profile = conn.profile.clone();
        let account = if prompt.is_passphrase {
            passphrase_account(&profile.id)
        } else {
            password_account(&profile.id)
        };

        if prompt.save_to_keychain && !secret.is_empty() {
            self.save_secret_then_connect(profile, account, secret, cx);
        } else {
            self.send_connect(profile, secret, false, cx);
        }
    }

    /// Save a secret to the keychain off-thread, then send `Connect` once the
    /// write completes (a failed save is a non-fatal toast — we still connect).
    fn save_secret_then_connect(
        &mut self,
        profile: Profile,
        account: String,
        secret: String,
        cx: &mut Context<Self>,
    ) {
        self.connecting_id = Some(profile.id.clone());
        let keyring = self.keyring;
        let secret_for_save = secret.clone();
        cx.spawn(async move |this, cx| {
            let saved =
                cx.background_executor()
                    .spawn(async move {
                        keyring.set_password(KEYCHAIN_SERVICE, &account, &secret_for_save)
                    })
                    .await;
            this.update(cx, |this, cx| {
                if saved.is_err() {
                    this.push_toast("Couldn't save secret to keychain", ToastVariant::Error, cx);
                }
                // A freshly-typed secret isn't a "stored" one for the auth-retry
                // heuristic, even though we just saved it.
                this.send_connect(profile, secret, false, cx);
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

    /// Subscribe to a modal field's [`TextInputEvent`]s so Enter submits and Esc
    /// dismisses the modal it belongs to. Dispatch is routed by *which* modal is
    /// open (mutually exclusive). The filter box is deliberately not wired.
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

    /// Toggle the sidebar **Recent** group's collapsed state.
    pub fn toggle_recent_collapsed(&mut self) {
        self.recent_collapsed = !self.recent_collapsed;
    }

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
            auth_is_key: false,
            auth_is_anonymous: false,
            key_path: cx.new(|cx| TextInput::new(cx).with_placeholder("~/.ssh/id_ed25519")),
            passphrase: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("Passphrase  (optional)")
                    .obscured()
            }),
            protocol: Protocol::Sftp,
            ftps_mode: FtpsMode::default(),
            color: AccentKind::Purple,
            test_status: None,
            testing: false,
        });
        self.wire_editor_inputs(cx);
        self.arm_editor_focus(cx);
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
        let (auth_is_key, auth_is_anonymous, key_path_str) = match &p.auth {
            AuthMethod::Password => (false, false, String::new()),
            AuthMethod::Key { path } => (true, false, path.display().to_string()),
            AuthMethod::Anonymous => (false, true, String::new()),
        };
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
            auth_is_key,
            auth_is_anonymous,
            key_path: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("~/.ssh/id_ed25519")
                    .with_content(key_path_str)
            }),
            passphrase: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("Leave blank to keep current")
                    .obscured()
            }),
            protocol: p.protocol,
            ftps_mode: p.ftps_mode,
            color: AccentKind::from_profile_color(p.color),
            test_status: None,
            testing: false,
        });
        self.wire_editor_inputs(cx);
        self.arm_editor_focus(cx);
    }

    /// Wire every editor field's submit/cancel events (Enter saves, Esc closes).
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
            editor.key_path.clone(),
            editor.passphrase.clone(),
        ];
        for input in &inputs {
            self.wire_input(input, cx);
        }
    }

    /// Switch the editor's auth method. Index 0 is always Password; index 1 is
    /// Key under SFTP and Anonymous under FTP/FTPS (the two are never both shown).
    pub fn set_editor_auth(&mut self, ix: usize) {
        if let Some(editor) = self.editor.as_mut() {
            let second_is_key = editor.protocol == Protocol::Sftp;
            editor.auth_is_key = ix == 1 && second_is_key;
            editor.auth_is_anonymous = ix == 1 && !second_is_key;
        }
    }

    /// Open a native file picker for the private key and write the chosen path
    /// into the editor's key-path field.
    pub fn pick_key_file(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose key".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                if let Some(editor) = this.editor.as_ref() {
                    editor.key_path.update(cx, |input, cx| {
                        input.set_content(path.display().to_string(), cx);
                    });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
        // Key auth is SFTP-only and anonymous is FTP/FTPS-only; a protocol switch
        // clears whichever no longer applies, leaving password auth as the default.
        if new == Protocol::Sftp {
            editor.auth_is_anonymous = false;
        } else {
            editor.auth_is_key = false;
        }
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

    /// Change the editor's FTPS TLS mode (0 = explicit, 1 = implicit). When the
    /// port still holds the protocol default, switch it to the conventional
    /// implicit/explicit port (990 / 21).
    pub fn set_editor_ftps_mode(&mut self, ix: usize, cx: &mut Context<Self>) {
        let new = if ix == 1 {
            FtpsMode::Implicit
        } else {
            FtpsMode::Explicit
        };
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        editor.ftps_mode = new;
        // Nudge the port to the conventional one when it still holds a default.
        let port_input = editor.port.clone();
        let port_text = port_input.read(cx).content().to_string();
        let port_trim = port_text.trim();
        if port_trim.is_empty() || port_trim == "21" || port_trim == "990" {
            let port = if new == FtpsMode::Implicit {
                "990"
            } else {
                "21"
            };
            port_input.update(cx, |input, cx| input.set_content(port, cx));
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
        let auth_is_key = editor.auth_is_key;
        let auth_is_anonymous = editor.auth_is_anonymous;
        let key_path = editor.key_path.read(cx).content().trim().to_string();
        let passphrase = editor.passphrase.read(cx).content().to_string();
        let protocol = editor.protocol;
        let ftps_mode = editor.ftps_mode;
        let color = editor.color;
        let id = editor.id.clone();
        let is_new = editor.is_new;

        // Anonymous login ignores the username; everything else requires one.
        if name.is_empty() || host.is_empty() || (username.is_empty() && !auth_is_anonymous) {
            self.push_toast(
                "Name, host and username are required",
                ToastVariant::Error,
                cx,
            );
            return;
        }
        if auth_is_key && key_path.is_empty() {
            self.push_toast("A key file is required", ToastVariant::Error, cx);
            return;
        }
        let auth = if auth_is_anonymous {
            AuthMethod::Anonymous
        } else if auth_is_key {
            AuthMethod::Key {
                path: key_path.into(),
            }
        } else {
            AuthMethod::Password
        };
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
            ftps_mode,
            host,
            port,
            username,
            auth,
            remote_path: (!remote_path.is_empty()).then_some(remote_path),
            color: color.to_profile_color(),
            last_connected,
        };
        if let Err(err) = self.store.save(&profile) {
            self.push_toast(err.to_string(), ToastVariant::Error, cx);
            return;
        }
        // Persist the secret by method. Anonymous has none — clear any entry left
        // from a prior password/key config so switching modes strands nothing.
        // Blank keeps whatever is already stored (the field shows "leave blank").
        if auth_is_anonymous {
            self.keyring_clear_async(id, cx);
        } else {
            let (secret, account) = if auth_is_key {
                (passphrase, passphrase_account(&id))
            } else {
                (password, password_account(&id))
            };
            if !secret.is_empty() {
                self.keyring_set_async(account, secret, cx);
            }
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

    /// Write a secret to the keychain off-thread, toasting on failure.
    fn keyring_set_async(&self, account: String, secret: String, cx: &mut Context<Self>) {
        let keyring = self.keyring;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { keyring.set_password(KEYCHAIN_SERVICE, &account, &secret) })
                .await;
            if res.is_err() {
                this.update(cx, |this, cx| {
                    this.push_toast("Couldn't save secret to keychain", ToastVariant::Error, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Best-effort, idempotent removal of a profile's keychain secrets — both the
    /// password and the key-passphrase entries (a profile may have written either).
    fn keyring_clear_async(&self, id: String, cx: &mut Context<Self>) {
        let keyring = self.keyring;
        cx.background_executor()
            .spawn(async move {
                let _ = keyring.delete_password(KEYCHAIN_SERVICE, &password_account(&id));
                let _ = keyring.delete_password(KEYCHAIN_SERVICE, &passphrase_account(&id));
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
        let auth_is_key = editor.auth_is_key;
        let auth_is_anonymous = editor.auth_is_anonymous;
        let key_path = editor.key_path.read(cx).content().trim().to_string();
        let passphrase = editor.passphrase.read(cx).content().to_string();
        let protocol = editor.protocol;
        let ftps_mode = editor.ftps_mode;
        let id = editor.id.clone();
        let is_new = editor.is_new;

        if host.is_empty() || (username.is_empty() && !auth_is_anonymous) {
            self.set_test_status(false, "Host and username are required");
            return;
        }
        if auth_is_key && key_path.is_empty() {
            self.set_test_status(false, "A key file is required");
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
        let auth = if auth_is_anonymous {
            AuthMethod::Anonymous
        } else if auth_is_key {
            AuthMethod::Key {
                path: key_path.into(),
            }
        } else {
            AuthMethod::Password
        };
        let profile = Profile {
            id: id.clone(),
            name: if name.is_empty() { "test".into() } else { name },
            protocol,
            ftps_mode,
            host,
            port,
            username,
            auth,
            remote_path: (!remote_path.is_empty()).then_some(remote_path),
            color: ProfileColor::default(),
            last_connected: None,
        };
        if let Some(editor) = self.editor.as_mut() {
            editor.testing = true;
            editor.test_status = None;
        }

        // Anonymous probes with no secret; skip the keychain entirely.
        if auth_is_anonymous {
            self.dispatch_test(profile, String::new());
            cx.notify();
            return;
        }
        // The secret to probe with (password or passphrase). On edit with a blank
        // field, fetch whatever is stored under the matching account.
        let (typed_secret, account) = if auth_is_key {
            (passphrase, passphrase_account(&id))
        } else {
            (password, password_account(&id))
        };
        if typed_secret.is_empty() && !is_new {
            let keyring = self.keyring;
            cx.spawn(async move |this, cx| {
                let res = cx
                    .background_executor()
                    .spawn(async move { keyring.get_password(KEYCHAIN_SERVICE, &account) })
                    .await;
                this.update(cx, |this, cx| {
                    let secret = res.ok().flatten().unwrap_or_default();
                    this.dispatch_test(profile, secret);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        } else {
            self.dispatch_test(profile, typed_secret);
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
    fn dispatch_test(&mut self, profile: Profile, secret: String) {
        let sent = self.service.send(Command::TestConnection {
            profile,
            secret: Secret::new(secret),
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
        self.arm_root_focus();
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
        self.arm_primary_focus();
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
        self.keyring_clear_async(id.clone(), cx);
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

    /// Open the file-row context menu. Right-click on an unselected row replaces
    /// the selection with just it; right-click inside the selection keeps it.
    pub fn open_file_menu(&mut self, name: SharedString, position: Point<Pixels>) {
        if !self.selected.contains(&name) {
            self.selected.clear();
            self.selected.insert(name.clone());
        }
        self.file_menu = Some(FileMenu { name, position });
        self.arm_root_focus();
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
        self.arm_input_focus(&input, cx);
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
    /// keyboard (F2) entry point that has no context menu to read.
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
        self.arm_input_focus(&input, cx);
        self.input_prompt = Some(InputPrompt {
            title: "Rename".into(),
            label: "New name".into(),
            submit_label: "Rename".into(),
            input,
            action: InputAction::Rename { original: name },
        });
    }

    /// Activate the current selection (the browser's Enter key): a single
    /// selected directory is opened, a symlink is resolved (navigate or
    /// download), otherwise the selection is downloaded.
    pub fn activate_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.len() == 1 {
            if let Some(name) = self.selected.iter().next().cloned() {
                match self.entry_kind(&name) {
                    Some(EntryKind::Directory) => {
                        self.open_dir(&name, cx);
                        return;
                    }
                    Some(EntryKind::Symlink) => {
                        self.open_symlink(&name, cx);
                        return;
                    }
                    _ => {}
                }
            }
        }
        self.download_selection(cx);
    }

    /// Activate one row by name (double-click): open a directory, resolve a
    /// symlink, or do nothing for a plain file (Enter/menu drive file actions).
    pub fn activate_row(&mut self, name: &SharedString, cx: &mut Context<Self>) {
        match self.entry_kind(name) {
            Some(EntryKind::Directory) => self.open_dir(name, cx),
            Some(EntryKind::Symlink) => self.open_symlink(name, cx),
            _ => {}
        }
    }

    /// The kind of a listed entry by name, if present.
    fn entry_kind(&self, name: &SharedString) -> Option<EntryKind> {
        self.listing
            .iter()
            .find(|row| row.entry.name.as_str() == name.as_ref())
            .map(|row| row.entry.kind)
    }

    /// Resolve a symlink on click: ask the backend to follow it. The reply
    /// ([`Event::SymlinkResolved`]) navigates into a directory target or
    /// downloads a file target — one round-trip, paid only on activation.
    pub fn open_symlink(&mut self, name: &SharedString, cx: &mut Context<Self>) {
        let path = self.cwd.join(name);
        if !self.service.send(Command::ResolveSymlink { path }) {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Dismiss the input modal without acting.
    pub fn cancel_input(&mut self) {
        self.input_prompt = None;
    }

    /// Validate and submit the input modal → `Mkdir` / `Rename`. Rejects an empty
    /// name or one containing `/`; an unchanged rename is a no-op.
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
                path: self.cwd.join(&value),
            },
            InputAction::Rename { original } => {
                if value == original.as_ref() {
                    return; // unchanged → nothing to do
                }
                Command::Rename {
                    from: self.cwd.join(&original),
                    to: self.cwd.join(&value),
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
        let entries: Vec<(SharedString, bool)> = self
            .selected
            .iter()
            .filter_map(|name| {
                self.listing
                    .iter()
                    .find(|row| row.entry.name.as_str() == name.as_ref())
                    .map(|row| (name.clone(), row.entry.is_dir()))
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        self.file_delete = Some(FileDeleteConfirm { entries });
        self.arm_primary_focus();
    }

    /// Dismiss the file-delete confirmation without deleting.
    pub fn cancel_file_delete(&mut self) {
        self.file_delete = None;
    }

    /// Issue one `Remove` per confirmed entry (file or recursive folder).
    pub fn confirm_file_delete(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.file_delete.take() else {
            return;
        };
        let mut ok = true;
        for (name, is_dir) in &confirm.entries {
            let path = self.cwd.join(name);
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

    /// Copy the single-selected entry's absolute remote path to the clipboard
    /// (keyboard `cmd-c`, mirroring the row menu's Copy path).
    pub fn copy_selection_path(&mut self, cx: &mut Context<Self>) {
        if self.selected.len() != 1 {
            return;
        }
        let Some(name) = self.selected.iter().next().cloned() else {
            return;
        };
        let path = self.cwd.join(&name);
        cx.write_to_clipboard(ClipboardItem::new_string(path.as_str().to_string()));
        self.push_toast("Path copied", ToastVariant::Success, cx);
    }

    /// Copy the clicked entry's absolute remote path to the clipboard.
    pub fn copy_path(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.file_menu.take() else {
            return;
        };
        let path = self.cwd.join(&menu.name);
        cx.write_to_clipboard(ClipboardItem::new_string(path.as_str().to_string()));
        self.push_toast("Path copied", ToastVariant::Success, cx);
    }

    /// Download the current selection. A single file opens a save-as dialog;
    /// anything else — several entries, or a single folder — opens a folder
    /// picker and issues one `Download` per top-level entry (folders recurse).
    pub fn download_selection(&mut self, cx: &mut Context<Self>) {
        self.close_file_menu();
        // (remote path, display name, is_dir) for each selected entry.
        let mut entries: Vec<(RemotePath, String, bool)> = Vec::new();
        for name in &self.selected {
            let Some(row) = self
                .listing
                .iter()
                .find(|row| row.entry.name.as_str() == name.as_ref())
            else {
                continue;
            };
            entries.push((self.cwd.join(name), name.to_string(), row.entry.is_dir()));
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
    fn download_remote_file(&mut self, remote: RemotePath, name: String, cx: &mut Context<Self>) {
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
    /// Returns whether the native session actually started — `false` when nothing
    /// was draggable (all symlinks/missing) or the platform refused — so the
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
    /// of `names`. Promotion is **one-way** — once the native session starts we
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
    fn update_drag_return_highlight(&mut self, p: Option<(f32, f32)>, cx: &mut Context<Self>) {
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
    /// of a drop-to-local — the Phase 3 re-entry case (the cursor can't be
    /// demoted back to an in-app drag, but the *drop* still becomes a move).
    fn on_drag_returned(
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

    /// Close the active connection and return to the welcome screen.
    pub fn disconnect(&mut self) {
        self.service.send(Command::Disconnect);
        self.active_id = None;
        self.online_id = None;
        self.connecting_id = None;
        self.view = View::Welcome;
        self.transfers.clear();
        self.pending_collisions.clear();
        self.collision_apply_all = false;
        self.set_listing(Vec::new());
        self.selected.clear();
        self.listing_loading = false;
        self.connection_lost = None;
        self.reconnect_attempt = None;
        self.reconnect_failed = false;
    }

    /// Reconnect after a connection loss: re-issue the connect for the active
    /// profile (re-fetching the secret from the keyring as on first connect). The
    /// connecting overlay covers the banner; the banner stays set underneath so a
    /// *failed* reconnect leaves it in place to retry. Success
    /// ([`Event::Connected`]) clears it and re-enters the browser.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_id.clone() else {
            return;
        };
        self.reconnect_attempt = None;
        self.reconnect_failed = false;
        self.open_connection(&id, cx);
    }

    /// Stop the in-progress auto-reconnect backoff loop, leaving the session lost
    /// with a manual-reconnect affordance (the banner drops its Cancel).
    pub fn cancel_reconnect(&mut self) {
        self.service.send(Command::CancelReconnect);
        self.reconnect_attempt = None;
        self.reconnect_failed = false;
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

    /// Copy a folder transfer's per-entry report to the clipboard as plain text
    /// — the whole point of surfacing the detail is that it can be pasted into a
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
                out.push_str(&format!("  {} — {}\n", issue.rel, issue.reason));
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

    /// Apply a backend [`Event`] to the state and request a redraw. This is the
    /// single sink for everything the service emits (see [`AppState::new`]).
    fn apply_event(&mut self, event: Event, cx: &mut Context<Self>) {
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
                    self.push_toast(format!("Folder finished — {note}"), ToastVariant::Info, cx);
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
                // A stored password that fails auth is likely stale — re-open the
                // prompt so the user can correct (and overwrite) it.
                if message.contains("authentication failed") {
                    if let Some(id) = stale {
                        if let Some(conn) = self.connections.iter().find(|c| c.profile.id == id) {
                            let profile = conn.profile.clone();
                            self.show_password_prompt(profile, cx);
                        }
                    }
                // An encrypted key with no/wrong passphrase — prompt for it.
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

    /// Enter the browser for a freshly-connected profile and list its starting
    /// directory: the profile's configured remote path if set, otherwise the
    /// server-resolved home (`home`) — so the user lands somewhere writable
    /// instead of the filesystem root.
    fn enter_browser(&mut self, profile_id: String, home: RemotePath, cx: &mut Context<Self>) {
        let configured = self
            .connections
            .iter()
            .find(|c| c.profile.id == profile_id)
            .and_then(|c| c.profile.remote_path.as_deref())
            .map(str::trim)
            .filter(|p| !p.is_empty());
        let root = match configured {
            Some(path) => RemotePath::new(path),
            None => home,
        };

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
        self.transfers = Vec::new();
        self.pending_collisions.clear();
        self.collision_apply_all = false;
        // Focus the file table so keyboard navigation works the moment we land.
        self.arm_focus(self.browser_focus.clone());
        self.reload_listing(cx);
    }

    /// Replace the current listing and refresh the cached visible order.
    fn set_listing(&mut self, listing: Vec<EntryRow>) {
        self.listing = Rc::new(listing);
        self.rebuild_view_order();
    }

    /// Request a listing for the current `cwd` from the backend. The result
    /// arrives asynchronously as an [`Event::DirListing`].
    fn reload_listing(&mut self, cx: &mut Context<Self>) {
        self.set_listing(Vec::new());
        self.listing_loading = true;
        if !self.service.send(Command::ListDir {
            path: self.cwd.clone(),
        }) {
            self.listing_loading = false;
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Navigate to a path, optionally pushing onto the history stack.
    fn go_to_path(&mut self, path: RemotePath, push_history: bool, cx: &mut Context<Self>) {
        self.cwd = path.clone();
        self.selected.clear();
        self.filter
            .update(cx, |input, cx| input.set_content("", cx));
        if push_history {
            self.history.truncate(self.history_ix + 1);
            self.history.push(path);
            self.history_ix = self.history.len() - 1;
        }
        self.reload_listing(cx);
    }

    /// Open a child directory by name.
    pub fn open_dir(&mut self, name: &SharedString, cx: &mut Context<Self>) {
        let path = self.cwd.join(name);
        self.go_to_path(path, true, cx);
    }

    /// Jump to the `n`-th breadcrumb (0 = root): rebuild the prefix from the
    /// first `n` components of the current path.
    pub fn nav_crumb(&mut self, n: usize, cx: &mut Context<Self>) {
        let mut path = RemotePath::root();
        for seg in self.cwd.components().take(n) {
            path = path.join(seg);
        }
        self.go_to_path(path, true, cx);
    }

    /// Go up one directory level.
    pub fn go_up(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self.cwd.parent() {
            self.go_to_path(parent, true, cx);
        }
    }

    pub fn can_back(&self) -> bool {
        self.history_ix > 0
    }

    pub fn can_forward(&self) -> bool {
        self.history_ix + 1 < self.history.len()
    }

    /// Step back in history.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        if !self.can_back() {
            return;
        }
        self.history_ix -= 1;
        let path = self.history[self.history_ix].clone();
        self.go_to_path(path, false, cx);
    }

    /// Step forward in history.
    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if !self.can_forward() {
            return;
        }
        self.history_ix += 1;
        let path = self.history[self.history_ix].clone();
        self.go_to_path(path, false, cx);
    }

    /// Refresh the current listing.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload_listing(cx);
    }

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
        self.rebuild_view_order();
    }

    /// The current filter text (lower-cased compare happens in the getter).
    pub fn filter_text(&self, cx: &App) -> String {
        self.filter.read(cx).content().to_string()
    }

    /// Pull the filter box text into [`filter_lower`](Self::filter_lower) and
    /// recompute the visible order. Called whenever the filter content changes.
    fn refilter(&mut self, cx: &App) {
        self.filter_lower = self.filter.read(cx).content().trim().to_lowercase();
        self.rebuild_view_order();
    }

    /// Recompute [`view_order`](Self::view_order) from the current listing, filter
    /// and sort. This is the one O(n log n) pass; it runs only on a data change,
    /// never per frame, and reuses each row's precomputed `name_lower` so name
    /// filtering/sorting allocates nothing.
    fn rebuild_view_order(&mut self) {
        let filter = &self.filter_lower;
        let mut order: Vec<usize> = self
            .listing
            .iter()
            .enumerate()
            .filter(|(_, row)| filter.is_empty() || row.name_lower.contains(filter.as_str()))
            .map(|(ix, _)| ix)
            .collect();

        let (key, asc) = self.sort;
        let listing = &self.listing;
        order.sort_by(|&a, &b| {
            let (a, b) = (&listing[a], &listing[b]);
            // Directories always sort before files.
            let dir_order = b.entry.is_dir().cmp(&a.entry.is_dir());
            if dir_order != std::cmp::Ordering::Equal {
                return dir_order;
            }
            let ord = match key {
                SortKey::Name => a.name_lower.cmp(&b.name_lower),
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
        self.view_order = Rc::new(order);
    }

    /// The indices into [`listing`](Self::listing) in visible order, shareable
    /// with the browser's `'static` row closures.
    pub fn view_order(&self) -> Rc<Vec<usize>> {
        self.view_order.clone()
    }

    /// Apply a row click: plain click replaces, cmd/ctrl-click toggles. Either
    /// way the clicked row becomes the anchor a later shift-click extends from.
    pub fn select(&mut self, name: SharedString, additive: bool) {
        if additive {
            if !self.selected.remove(&name) {
                self.selected.insert(name.clone());
            }
        } else {
            self.selected.clear();
            self.selected.insert(name.clone());
        }
        self.select_anchor = Some(name);
    }

    /// Apply a shift-click: select the inclusive range from the anchor row to the
    /// clicked row in the current visible order. With no (visible) anchor it
    /// behaves like a plain click. The anchor is left where it was so successive
    /// shift-clicks re-extend from the same origin.
    pub fn select_range(&mut self, name: SharedString) {
        let names = self.visible_names();
        let clicked = names.iter().position(|n| *n == name);
        let anchor = self
            .select_anchor
            .as_ref()
            .and_then(|a| names.iter().position(|n| n == a));
        match (clicked, anchor) {
            (Some(click), Some(anchor)) => {
                let (lo, hi) = (click.min(anchor), click.max(anchor));
                self.selected = names[lo..=hi].iter().cloned().collect();
            }
            // No anchor (or it scrolled out of the listing): fall back to a plain
            // select, seeding the anchor for the next shift-click.
            _ => self.select(name, false),
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
                TransferStatus::Running
                | TransferStatus::Queued
                | TransferStatus::AwaitingDecision
                | TransferStatus::Interrupted => counts.1 += 1,
                TransferStatus::Completed => counts.2 += 1,
                TransferStatus::Failed => counts.3 += 1,
                TransferStatus::Cancelled | TransferStatus::Skipped => {}
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
                TransferStatus::Running
                    | TransferStatus::Queued
                    | TransferStatus::AwaitingDecision
                    | TransferStatus::Interrupted
            )
        });
    }

    /// Whether any overlay — a modal, prompt, context menu, the cheat-sheet, or
    /// the connecting spinner — is currently on screen. The browser drops its key
    /// context while this holds, so global Enter/Esc route to the overlay instead
    /// of the file table beneath it.
    pub fn has_overlay(&self) -> bool {
        self.editor.is_some()
            || self.password_prompt.is_some()
            || self.host_key_prompt.is_some()
            || !self.pending_collisions.is_empty()
            || self.delete_confirm.is_some()
            || self.file_delete.is_some()
            || self.input_prompt.is_some()
            || self.tweaks_open
            || self.shortcuts_open
            || self.row_menu.is_some()
            || self.file_menu.is_some()
            || self.connecting_id.is_some()
    }

    /// Toggle the sidebar's visibility (the `cmd-b` global).
    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
    }

    /// Open the Tweaks (settings) modal (`cmd-,`).
    pub fn open_settings(&mut self) {
        self.tweaks_open = true;
        self.arm_primary_focus();
    }

    /// Toggle the keyboard-shortcuts cheat-sheet (`cmd-/`).
    pub fn toggle_shortcuts(&mut self) {
        self.shortcuts_open = !self.shortcuts_open;
        if self.shortcuts_open {
            self.arm_root_focus();
        }
    }

    /// Esc handler: dismiss the topmost overlay — menus first, then the cheat
    /// sheet, then prompts/modals in z-order. Returns whether anything closed.
    /// Each dismissal is the modal's own cancel (e.g. a collision Skip), never a
    /// destructive default.
    pub fn dismiss_topmost_overlay(&mut self, cx: &mut Context<Self>) -> bool {
        if self.row_menu.is_some() {
            self.row_menu = None;
        } else if self.file_menu.is_some() {
            self.file_menu = None;
        } else if self.shortcuts_open {
            self.shortcuts_open = false;
        } else if self.editor.is_some() {
            self.close_editor();
        } else if self.password_prompt.is_some() {
            self.cancel_password();
        } else if self.host_key_prompt.is_some() {
            self.reject_host_key();
        } else if !self.pending_collisions.is_empty() {
            self.resolve_collision(CollisionChoice::Skip, cx);
        } else if self.delete_confirm.is_some() {
            self.cancel_delete();
        } else if self.file_delete.is_some() {
            self.cancel_file_delete();
        } else if self.input_prompt.is_some() {
            self.cancel_input();
        } else if self.tweaks_open {
            self.tweaks_open = false;
            self.theme_select_open = false;
        } else {
            return false;
        }
        true
    }

    /// The visible (filtered + sorted) entry names, in display order.
    fn visible_names(&self) -> Vec<SharedString> {
        self.view_order
            .iter()
            .map(|&ix| SharedString::from(self.listing[ix].entry.name.clone()))
            .collect()
    }

    /// Move the single-row selection by `delta` rows (keyboard up/down). With no
    /// selection, down picks the first row and up the last.
    pub fn move_selection(&mut self, delta: i32) {
        let names = self.visible_names();
        if names.is_empty() {
            return;
        }
        let next = match names.iter().position(|n| self.selected.contains(n)) {
            Some(cur) => (cur as i32 + delta).clamp(0, names.len() as i32 - 1) as usize,
            None if delta >= 0 => 0,
            None => names.len() - 1,
        };
        self.selected.clear();
        self.selected.insert(names[next].clone());
    }

    /// Select the first (`last == false`) or last row (Home / End).
    pub fn select_edge(&mut self, last: bool) {
        let names = self.visible_names();
        let Some(target) = (if last { names.last() } else { names.first() }) else {
            return;
        };
        let target = target.clone();
        self.selected.clear();
        self.selected.insert(target);
    }

    /// Select every visible row (`cmd-a` in the file table).
    pub fn select_all_visible(&mut self) {
        self.selected = self.visible_names().into_iter().collect();
    }

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

/// The keychain account holding a profile's connection secret — the password for
/// password auth, the key passphrase for key auth.
fn secret_account(profile: &Profile) -> String {
    match profile.auth {
        AuthMethod::Password => password_account(&profile.id),
        AuthMethod::Key { .. } => passphrase_account(&profile.id),
        // Anonymous has no stored secret; callers short-circuit before reaching here.
        AuthMethod::Anonymous => password_account(&profile.id),
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
