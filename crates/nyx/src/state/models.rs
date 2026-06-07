//! View-models and presentation helpers.
//!
//! `nyx-core` domain types are not extended with UI fields; the app wraps them
//! in thin view-models carrying presentation-only state (accent color, "recent"
//! flag, speed/error). Display strings are computed here, never stored.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{Hsla, SharedString};
use nyx_core::{EntryKind, Protocol, RemoteEntry, Transfer, TransferReport, TransferStatus};
use nyx_profile::{Profile, ProfileColor};
use nyx_ui::{BadgeVariant, Theme};
use time::OffsetDateTime;

/// A profile counts as "recent" if it was last connected within this many days.
const RECENT_DAYS: i64 = 30;

/// A connection profile plus its UI-only presentation state, all derived from
/// the persisted profile so they survive a restart.
pub struct ConnectionVm {
    /// The real domain profile.
    pub profile: Profile,
    /// Accent color shown on the connection's icon / dot.
    pub color: AccentKind,
    /// Human "last used" label (e.g. `"4m ago"`); `None` if never connected.
    pub last_used: Option<SharedString>,
    /// Whether this profile appears in the "Recent" group.
    pub is_recent: bool,
}

impl ConnectionVm {
    /// Build a view-model from a stored profile, deriving the presentation
    /// fields (accent color, relative "last used" label, recency).
    pub fn from_profile(profile: Profile) -> Self {
        let color = AccentKind::from_profile_color(profile.color);
        let (last_used, is_recent) = match profile.last_connected {
            Some(ts) => {
                let recent = (OffsetDateTime::now_utc() - ts).whole_days() <= RECENT_DAYS;
                (Some(fmt_relative(ts).into()), recent)
            }
            None => (None, false),
        };
        Self {
            profile,
            color,
            last_used,
            is_recent,
        }
    }

    /// `user@host` — the faint mono subtitle in the sidebar.
    pub fn user_host(&self) -> String {
        format!("{}@{}", self.profile.username, self.profile.host)
    }

    /// `user@host:port` — the full host label (welcome card, status bar).
    pub fn user_host_port(&self) -> String {
        format!(
            "{}@{}:{}",
            self.profile.username, self.profile.host, self.profile.port
        )
    }
}

/// A coarse "x ago" label for a past timestamp (sidebar / welcome "Recent").
fn fmt_relative(ts: OffsetDateTime) -> String {
    let secs = (OffsetDateTime::now_utc() - ts).whole_seconds().max(0);
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// A transfer plus its UI-only presentation state.
pub struct TransferVm {
    /// The real domain transfer (progress reads straight off this).
    pub transfer: Transfer,
    /// Live transfer speed in bytes/sec, from `Event::TransferProgress`.
    pub speed_bps: Option<u64>,
    /// Display copy for a failed transfer.
    pub error: Option<SharedString>,
    /// Per-entry detail for a folder that completed with skips/failures; `None`
    /// for clean folders and file transfers.
    pub report: Option<TransferReport>,
    /// Whether the dock row's per-entry report is expanded.
    pub report_expanded: bool,
}

/// A directory entry plus its derived display strings.
pub struct EntryRow {
    /// The real domain entry.
    pub entry: RemoteEntry,
    /// Human type label, e.g. `"Folder"`, `"JavaScript"`, `"PNG image"`.
    pub type_label: SharedString,
}

impl EntryRow {
    /// Build a row from a domain entry, deriving its type label.
    pub fn new(entry: RemoteEntry) -> Self {
        let type_label = match entry.kind {
            EntryKind::Directory => "Folder".into(),
            EntryKind::Symlink => "Link".into(),
            // `file_kind` adds the extension nuance (e.g. "JavaScript").
            EntryKind::File | EntryKind::Other => file_kind(&entry.name).into(),
        };
        Self { entry, type_label }
    }

    /// Display size: `"—"` for directories, otherwise a human byte size.
    pub fn display_size(&self) -> SharedString {
        if self.entry.is_dir() {
            "—".into()
        } else {
            fmt_size(self.entry.size).into()
        }
    }

    /// Display modified time (mono), or `"—"` when unknown.
    pub fn display_modified(&self) -> SharedString {
        fmt_modified(self.entry.modified).into()
    }

    /// The `ls -l`-style permission string with a leading type char, e.g.
    /// `"drwxr-xr-x"`. The type prefix is derived from `kind` (UI concern), not
    /// stored.
    pub fn display_perms(&self) -> SharedString {
        let type_char = match self.entry.kind {
            EntryKind::Directory => 'd',
            EntryKind::Symlink => 'l',
            EntryKind::File | EntryKind::Other => '-',
        };
        format!("{type_char}{}", self.entry.permissions.rwx_string()).into()
    }

    /// The icon name + color for this entry, against the active theme.
    pub fn icon(&self, theme: &Theme) -> (&'static str, Hsla) {
        match self.entry.kind {
            EntryKind::Directory => ("folder", theme.blue),
            EntryKind::Symlink => ("link", theme.text_muted),
            EntryKind::File | EntryKind::Other => file_icon(&self.entry.name, theme),
        }
    }
}

/// Connection accent colors (UI-only). Mapped to theme tokens at render time and
/// to/from the persisted [`ProfileColor`] (the app's color picker uses these).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccentKind {
    Blue,
    Purple,
    Green,
}

impl AccentKind {
    /// The accent kinds the color picker offers, in display order.
    pub const ALL: [AccentKind; 3] = [AccentKind::Blue, AccentKind::Purple, AccentKind::Green];

    /// Resolve to a concrete color against the active theme.
    pub fn color(self, theme: &Theme) -> Hsla {
        match self {
            AccentKind::Blue => theme.blue,
            AccentKind::Purple => theme.purple,
            AccentKind::Green => theme.green,
        }
    }

    /// Map a persisted [`ProfileColor`] to its UI accent.
    pub fn from_profile_color(color: ProfileColor) -> Self {
        match color {
            ProfileColor::Blue => AccentKind::Blue,
            ProfileColor::Purple => AccentKind::Purple,
            ProfileColor::Green => AccentKind::Green,
        }
    }

    /// Map back to the persisted [`ProfileColor`].
    pub fn to_profile_color(self) -> ProfileColor {
        match self {
            AccentKind::Blue => ProfileColor::Blue,
            AccentKind::Purple => ProfileColor::Purple,
            AccentKind::Green => ProfileColor::Green,
        }
    }

    /// The picker index for this accent.
    pub fn index(self) -> usize {
        match self {
            AccentKind::Blue => 0,
            AccentKind::Purple => 1,
            AccentKind::Green => 2,
        }
    }
}

/// The badge variant + short label for a protocol.
pub fn protocol_badge(protocol: Protocol) -> (BadgeVariant, &'static str) {
    match protocol {
        Protocol::Sftp => (BadgeVariant::Special, "SFTP"),
        Protocol::Ftps => (BadgeVariant::Success, "FTPS"),
        Protocol::Ftp => (BadgeVariant::Info, "FTP"),
    }
}

/// A sortable file-table column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

impl SortKey {
    /// The table column index this key occupies.
    pub fn column(self) -> usize {
        match self {
            SortKey::Name => 0,
            SortKey::Size => 1,
            SortKey::Modified => 2,
            SortKey::Kind => 3,
        }
    }

    /// The sort key for a column index, if it is sortable.
    pub fn from_column(ix: usize) -> Option<Self> {
        match ix {
            0 => Some(SortKey::Name),
            1 => Some(SortKey::Size),
            2 => Some(SortKey::Modified),
            3 => Some(SortKey::Kind),
            _ => None,
        }
    }
}

/// The transfer-dock filter tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DockTab {
    All,
    /// Running + queued.
    Active,
    Completed,
    Failed,
}

impl DockTab {
    /// Tabs in display order.
    pub const ALL: [DockTab; 4] = [
        DockTab::All,
        DockTab::Active,
        DockTab::Completed,
        DockTab::Failed,
    ];

    /// The tab for a selected index (defaults to `All`).
    pub fn from_index(ix: usize) -> Self {
        Self::ALL.get(ix).copied().unwrap_or(DockTab::All)
    }

    /// The tab's selected index.
    pub fn index(self) -> usize {
        match self {
            DockTab::All => 0,
            DockTab::Active => 1,
            DockTab::Completed => 2,
            DockTab::Failed => 3,
        }
    }

    /// Whether a transfer status belongs under this tab.
    pub fn matches(self, status: TransferStatus) -> bool {
        match self {
            DockTab::All => true,
            DockTab::Active => {
                matches!(
                    status,
                    TransferStatus::Running
                        | TransferStatus::Queued
                        | TransferStatus::AwaitingDecision
                )
            }
            DockTab::Completed => status == TransferStatus::Completed,
            DockTab::Failed => status == TransferStatus::Failed,
        }
    }
}

/// File-row vertical density (exercises `Table::row_height`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Density {
    /// 22px rows.
    Compact,
    /// 26px rows (default).
    Comfortable,
    /// 30px rows.
    Spacious,
}

impl Density {
    /// Densities in display order.
    pub const ALL: [Density; 3] = [Density::Compact, Density::Comfortable, Density::Spacious];

    /// Row height in pixels.
    pub fn row_height(self) -> f32 {
        match self {
            Density::Compact => 22.0,
            Density::Comfortable => 26.0,
            Density::Spacious => 30.0,
        }
    }

    /// The control's selected index.
    pub fn index(self) -> usize {
        match self {
            Density::Compact => 0,
            Density::Comfortable => 1,
            Density::Spacious => 2,
        }
    }
}

/// Human-readable byte size (`"—"` for zero, matching the design).
pub fn fmt_size(bytes: u64) -> String {
    if bytes == 0 {
        return "—".to_string();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut n = bytes as f64;
    let mut i = 0;
    while n >= 1024.0 && i < UNITS.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    let decimals = if n < 10.0 && i > 0 { 1 } else { 0 };
    format!("{n:.decimals$} {}", UNITS[i])
}

/// A transfer's transferred/total bytes pair, e.g. `"540 KB / 862 KB"`.
pub fn fmt_bytes_pair(t: &Transfer) -> String {
    match t.status {
        TransferStatus::Completed => fmt_size(t.total_bytes.unwrap_or(t.transferred_bytes)),
        TransferStatus::Queued => "0 B".to_string(),
        _ => match t.total_bytes {
            Some(total) => format!("{} / {}", fmt_size(t.transferred_bytes), fmt_size(total)),
            None => fmt_size(t.transferred_bytes),
        },
    }
}

/// Format a modified time as `"Jun  4  22:09"` (mono), or `"—"` when unknown.
pub fn fmt_modified(time: Option<SystemTime>) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let Some(time) = time else {
        return "—".to_string();
    };
    let Ok(dur) = time.duration_since(UNIX_EPOCH) else {
        return "—".to_string();
    };
    let secs = dur.as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (_year, month, day) = civil_from_days(days);
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    format!("{} {day:>2}  {hh:02}:{mm:02}", MONTHS[(month - 1) as usize])
}

/// Civil date (year, month 1-12, day 1-31) from days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Human file-type label from a file name (mirrors the prototype's `fileKind`).
fn file_kind(name: &str) -> &'static str {
    if name.ends_with(".tar.gz") || name.ends_with(".sql.gz") {
        return "Gzip archive";
    }
    if name.ends_with(".tar.zst") {
        return "Zstd archive";
    }
    if name.ends_with(".js.map") {
        return "Source Map";
    }
    if name.starts_with(".env") {
        return "Env file";
    }
    let ext = name.rsplit('.').next().unwrap_or("");
    match ext {
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" | "jsx" => "TypeScript",
        "css" | "scss" | "sass" | "less" => "Stylesheet",
        "html" | "htm" => "HTML",
        "json" => "JSON",
        "toml" | "yml" | "yaml" => "Config",
        "png" => "PNG image",
        "webp" => "WebP image",
        "jpg" | "jpeg" => "JPEG image",
        "gif" => "GIF image",
        "svg" => "SVG image",
        "ico" => "Icon",
        "pdf" => "PDF",
        "csv" => "CSV",
        "log" => "Log file",
        "conf" | "config" => "Config",
        "sh" => "Shell script",
        "sql" => "SQL",
        "gz" => "Gzip archive",
        "zst" => "Zstd archive",
        "zip" => "Zip archive",
        "tar" => "Tar archive",
        "woff2" | "woff" | "ttf" => "Font",
        "mp4" | "webm" | "mov" | "mkv" => "Video",
        "bashrc" | "profile" => "Shell config",
        _ => "File",
    }
}

/// Pick a file-type icon name + color from a file name and theme.
fn file_icon(name: &str, theme: &Theme) -> (&'static str, Hsla) {
    let lower = name.to_ascii_lowercase();
    let ends = |exts: &[&str]| exts.iter().any(|e| lower.ends_with(e));
    if ends(&[".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx"]) || lower.ends_with(".js.map") {
        ("fileCode", theme.yellow)
    } else if ends(&[".css", ".scss", ".sass", ".less"]) {
        ("fileCode", theme.blue)
    } else if ends(&[
        ".html", ".htm", ".xml", ".conf", ".sh", ".json", ".toml", ".yml", ".yaml",
    ]) || lower.starts_with(".env")
    {
        ("fileCode", theme.green)
    } else if ends(&[".png", ".jpg", ".jpeg", ".webp", ".gif", ".svg", ".ico"]) {
        ("fileImage", theme.purple)
    } else if ends(&[".zip", ".gz", ".zst", ".tar", ".7z", ".rar", ".tgz"])
        || lower.contains(".tar.")
    {
        ("fileArchive", theme.orange)
    } else if lower.ends_with(".log") {
        ("fileLog", theme.text_faint)
    } else if ends(&[".mp4", ".webm", ".mov", ".mkv"]) {
        ("fileImage", theme.red)
    } else if ends(&[".pdf", ".csv", ".sql"]) {
        ("file", theme.text_muted)
    } else {
        ("file", theme.text_faint)
    }
}
