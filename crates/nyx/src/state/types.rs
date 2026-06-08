//! Prompt, menu, editor, and search state types used by [`AppState`].

use super::*;

/// Which top-level screen the main column shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    /// The welcome / connection-manager screen.
    Welcome,
    /// The file browser for the active connection.
    Browse,
}

/// Which category the settings panel's left nav has selected.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SettingsTab {
    /// Theme + file-browser display.
    #[default]
    Appearance,
    /// Connection behavior.
    Connection,
    /// App version and links.
    About,
}

impl SettingsTab {
    /// Every tab, in nav order.
    pub const ALL: [SettingsTab; 3] = [
        SettingsTab::Appearance,
        SettingsTab::Connection,
        SettingsTab::About,
    ];

    /// The nav label / page title.
    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::Appearance => "Appearance",
            SettingsTab::Connection => "Connection",
            SettingsTab::About => "About",
        }
    }
}

/// An in-progress rubber-band (rectangle) selection in the file table. Begun by
/// a left-press on empty space (never over a row, so it can't fight a file grab),
/// it grows with the pointer and selects every row its rect crosses. Coordinates
/// are GPUI window coordinates, matching the painted row rects it hit-tests.
pub struct Marquee {
    /// Where the press began (the fixed corner).
    pub origin: Point<Pixels>,
    /// The current pointer position (the moving corner).
    pub current: Point<Pixels>,
    /// Whether the pointer has moved past the start threshold. Until then the
    /// rectangle isn't drawn, so a plain click doesn't flash a zero-size box.
    pub active: bool,
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
/// `AppState`. It prompts for a password or - when [`is_passphrase`] - a key
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
    /// The profile id - freshly generated on create, preserved on edit.
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

/// A reusable single-field input modal - shared by **New folder** (blank) and
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

/// An active recursive tree search (the filter box in `/`-scope). Holds the
/// streamed-in hits and the walk's progress, and swaps the file table for a
/// results view while present.
pub struct SearchState {
    /// Correlates streamed [`Event::SearchResult`] batches; stale tokens dropped.
    pub(super) token: u64,
    /// The raw filter text (e.g. `/*.rs`), shown in the results header.
    pub query: SharedString,
    /// Display-ready hits accumulated so far, behind an `Rc` so the browser's
    /// `'static` row closures share them without cloning per frame.
    pub hits: Rc<Vec<SearchRow>>,
    /// Whether the backend signaled the walk finished (or was capped).
    pub done: bool,
    /// Whether the result cap stopped the walk before the tree was exhausted.
    pub truncated: bool,
}

/// A tree-search hit, display-ready: the entry (for icon/size/type) plus its
/// parent path (the Path column) and full path (navigation on activation).
#[derive(Clone)]
pub struct SearchRow {
    /// The matched entry, wrapped for its display helpers.
    pub row: EntryRow,
    /// The hit's parent directory, shown in the Path column.
    pub parent: SharedString,
    /// The hit's absolute path - `go_to_path`'d when the row is opened.
    pub path: RemotePath,
}

impl SearchRow {
    pub(super) fn from_hit(hit: SearchHit) -> Self {
        let parent = hit
            .path
            .parent()
            .map(|p| p.as_str().to_string())
            .unwrap_or_default();
        Self {
            row: EntryRow::new(hit.entry),
            parent: parent.into(),
            path: hit.path,
        }
    }
}
