//! [`AppState`] - the single source of truth for the app shell.
//!
//! One root `Entity<AppState>` holds all mutable state plus the interaction
//! logic (navigation, sort, filter, selection, dock). Views are `RenderOnce`
//! helpers that read a `&AppState` and emit elements; only the filter
//! [`TextInput`] is its own entity (it needs focus/IME state).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::channel::oneshot;
use futures::StreamExt;
use gpui::{
    point, prelude::*, px, App, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    PathPromptOptions, Pixels, Point, SharedString, Window,
};
use nyx_core::{
    CollisionChoice, EntryKind, EntryOutcomeKind, Filter, FtpsMode, Protocol, RemotePath, Scope,
    Secret, ServerTrustKind, Transfer, TransferDirection, TransferId, TransferKind, TransferStatus,
};
use nyx_drag::DragFile;
use nyx_keyring::{passphrase_account, password_account, CredentialStore, OsKeyring};
use nyx_profile::{
    AuthMethod, FileProfileStore, FileSettingsStore, Profile, ProfileColor, ProfileStore, Settings,
};
use nyx_service::{Command, Event, FileOp, SearchHit, ServiceHandle};
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

mod connection;
mod dock;
mod editor;
mod event;
mod file_ops;
mod focus;
mod lifecycle;
pub mod models;
mod navigation;
mod overlay;
mod profile_menu;
mod query;
mod selection;
mod transfer;
mod types;

pub use types::*;

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
    /// by [`filter_query`](Self::filter_query), then sorted, folders first).
    /// Rebuilt only when the listing, sort, or filter changes - never per frame.
    view_order: Rc<Vec<usize>>,
    /// The stateful filter box.
    pub filter: Entity<TextInput>,
    /// The parsed filter query, kept in sync with [`filter`](Self::filter) so
    /// [`rebuild_view_order`](Self::rebuild_view_order) needs no `cx`.
    filter_query: Filter,
    /// Active recursive tree search, when the filter is in `/`-scope. `None` while
    /// browsing - the file table renders the current directory; `Some` swaps it
    /// for streamed search results.
    search: Option<SearchState>,
    /// Monotonic token; each new tree search bumps it so stale streamed batches
    /// (from a superseded search) are dropped.
    search_seq: u64,
    /// An entry to select once its directory listing arrives - set when a search
    /// hit is activated, so landing in its folder lands on the file too.
    pending_select: Option<SharedString>,
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
    /// A focus target to apply on the next render - modal autofocus, focusing the
    /// file table on connect, etc. Consumed once.
    pending_focus: Option<FocusHandle>,
    /// Handle for the open modal's primary button. Field-less modals autofocus it
    /// so Enter activates the default action (GPUI fires the focused button's
    /// click natively - no separate confirm action that would double-fire).
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
    /// under the cursor - highlighted so the (unchangeable native) cursor still
    /// has a visible drop target. `None` when outside or over a non-folder.
    pub drag_return_folder: Option<SharedString>,
    /// The profile id of an in-flight connection attempt, if any.
    pub connecting_id: Option<String>,
    /// The profile id whose connect used a *stored* password - set so an auth
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
    /// reason that drives the non-modal "Connection lost - Reconnect" banner.
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

/// The keychain account holding a profile's connection secret - the password for
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
