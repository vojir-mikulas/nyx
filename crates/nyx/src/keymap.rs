//! The app's single source of truth for keyboard bindings.
//!
//! [`bind_all`] is the one registration entry point (called from `main`). It
//! orchestrates: it invokes [`TextInput::bind_keys`] for the in-house editing set
//! (which must stay in `flint`), then registers every app/browser binding from
//! the [`SHORTCUTS`] table. That same table drives the shortcuts cheat-sheet
//! ([`cheat_sheet`]), so the overlay can never drift from the real bindings.
//!
//! ## Context hierarchy (why the table works)
//!
//! GPUI dispatches a keystroke to the binding matching at the *deepest* context
//! in the focused element's stack (root `"App"`, then `"Browser"`, then
//! `"TextInput"`). Binding a global key under the `"App"` identifier matches at
//! the shallow root, so a deeper `"Browser"` or `"TextInput"` binding of the same
//! key shadows it: `enter` is *Open* over the file table and *Submit* in a field.
//! A `None`-context binding, by contrast, binds at max depth and fires even
//! inside fields - the footgun this module exists to avoid.
//!
//! Modal Enter/Space are *not* bound here: GPUI fires the focused (or autofocused
//! primary) button's click natively, so a single keystroke can't both activate a
//! button and run a separate confirm action. Only `escape` (→ [`Dismiss`]) is a
//! modal-level binding.

use flint::TextInput;
use gpui::{App, KeyBinding};

use crate::views::browser::{
    CopyPath, Delete, GoUp, Open, Rename, SelectAllRows, SelectDown, SelectFirst, SelectLast,
    SelectUp,
};
use crate::views::welcome::ActivateRow;

gpui::actions!(
    nyx,
    [
        /// Open the connection editor in Create mode.
        NewConnection,
        /// Show or hide the sidebar.
        ToggleSidebar,
        /// Focus the browser's filter field.
        FocusFilter,
        /// Close the active connection.
        CloseTab,
        /// Reload the current directory listing.
        Refresh,
        /// Open the Tweaks (settings) modal.
        OpenSettings,
        /// Open the keyboard-shortcuts cheat-sheet.
        ShowShortcuts,
        /// Quit the application.
        Quit,
        /// Move focus to the next focusable item (Tab).
        FocusNext,
        /// Move focus to the previous focusable item (Shift-Tab).
        FocusPrev,
        /// Dismiss the topmost overlay - modal, prompt or menu (Esc).
        Dismiss,
    ]
);

/// The cheat-sheet section a binding is listed under (display only - distinct
/// from the binding's key context).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Application,
    Browser,
    Dialogs,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Application => "Application",
            Group::Browser => "Browser",
            Group::Dialogs => "Dialogs",
        }
    }

    const ALL: [Group; 3] = [Group::Application, Group::Browser, Group::Dialogs];
}

/// Builds a `KeyBinding` for a (possibly platform-mapped) keystroke and context.
type MakeFn = fn(&str, Option<&str>) -> KeyBinding;

/// One row of the keymap: the canonical keystroke (with `cmd` as the
/// platform-primary modifier), a human label, the cheat-sheet group, the binding
/// key context, and a constructor that builds the `KeyBinding`.
struct Shortcut {
    keys: &'static str,
    label: &'static str,
    group: Group,
    context: Option<&'static str>,
    make: MakeFn,
}

/// The full set of app/browser bindings - the single source of truth shared by
/// [`bind_all`] and [`cheat_sheet`].
#[rustfmt::skip]
const SHORTCUTS: &[Shortcut] = &[
    // Application
    Shortcut { keys: "cmd-n", label: "New connection",     group: Group::Application, context: Some("App"), make: |k, c| KeyBinding::new(k, NewConnection, c) },
    Shortcut { keys: "cmd-b", label: "Toggle sidebar",     group: Group::Application, context: Some("App"), make: |k, c| KeyBinding::new(k, ToggleSidebar, c) },
    Shortcut { keys: "cmd-,", label: "Settings",           group: Group::Application, context: Some("App"), make: |k, c| KeyBinding::new(k, OpenSettings, c) },
    Shortcut { keys: "cmd-/", label: "Keyboard shortcuts", group: Group::Application, context: Some("App"), make: |k, c| KeyBinding::new(k, ShowShortcuts, c) },
    Shortcut { keys: "tab",       label: "Next item",     group: Group::Application, context: Some("App"), make: |k, c| KeyBinding::new(k, FocusNext, c) },
    Shortcut { keys: "shift-tab", label: "Previous item", group: Group::Application, context: Some("App"), make: |k, c| KeyBinding::new(k, FocusPrev, c) },

    // Browser - navigation/global (App context so they don't fight text fields)
    Shortcut { keys: "cmd-f", label: "Filter folder",      group: Group::Browser, context: Some("App"), make: |k, c| KeyBinding::new(k, FocusFilter, c) },
    Shortcut { keys: "cmd-r", label: "Refresh",            group: Group::Browser, context: Some("App"), make: |k, c| KeyBinding::new(k, Refresh, c) },
    Shortcut { keys: "cmd-w", label: "Close connection",   group: Group::Browser, context: Some("App"), make: |k, c| KeyBinding::new(k, CloseTab, c) },
    // Browser - table actions (Browser context; live only while the table is focused)
    Shortcut { keys: "enter",     label: "Open / download", group: Group::Browser, context: Some("Browser"), make: |k, c| KeyBinding::new(k, Open, c) },
    Shortcut { keys: "backspace", label: "Go up",           group: Group::Browser, context: Some("Browser"), make: |k, c| KeyBinding::new(k, GoUp, c) },
    Shortcut { keys: "f2",        label: "Rename",          group: Group::Browser, context: Some("Browser"), make: |k, c| KeyBinding::new(k, Rename, c) },
    Shortcut { keys: "delete",    label: "Delete",          group: Group::Browser, context: Some("Browser"), make: |k, c| KeyBinding::new(k, Delete, c) },
    Shortcut { keys: "up",        label: "Move up",         group: Group::Browser, context: Some("Browser"), make: |k, c| KeyBinding::new(k, SelectUp, c) },
    Shortcut { keys: "down",      label: "Move down",       group: Group::Browser, context: Some("Browser"), make: |k, c| KeyBinding::new(k, SelectDown, c) },
    Shortcut { keys: "home",      label: "First item",      group: Group::Browser, context: Some("Browser"), make: |k, c| KeyBinding::new(k, SelectFirst, c) },
    Shortcut { keys: "end",       label: "Last item",       group: Group::Browser, context: Some("Browser"), make: |k, c| KeyBinding::new(k, SelectLast, c) },
    Shortcut { keys: "cmd-a",     label: "Select all",      group: Group::Browser, context: Some("Browser"), make: |k, c| KeyBinding::new(k, SelectAllRows, c) },
    Shortcut { keys: "cmd-c",     label: "Copy path",       group: Group::Browser, context: Some("Browser"), make: |k, c| KeyBinding::new(k, CopyPath, c) },

    // Dialogs
    Shortcut { keys: "escape", label: "Cancel", group: Group::Dialogs, context: Some("App"), make: |k, c| KeyBinding::new(k, Dismiss, c) },
];

/// Alias keystrokes that are bound but kept out of the cheat-sheet to avoid
/// listing two keys for one action.
#[rustfmt::skip]
const ALIASES: &[(&str, Option<&str>, MakeFn)] = &[
    ("cmd-backspace", Some("Browser"), |k, c| KeyBinding::new(k, Delete, c)),
    // Activate the focused welcome-list row (Enter); shadows the App-level Confirm
    // because the row's `"ConnRow"` context is deeper.
    ("enter", Some("ConnRow"), |k, c| KeyBinding::new(k, ActivateRow, c)),
];

/// Register every keyboard binding. Call once at startup.
pub fn bind_all(cx: &mut App) {
    // The in-house editing set stays in flint (it must not depend on nyx-*).
    TextInput::bind_keys(cx);

    let mut bindings: Vec<KeyBinding> = SHORTCUTS
        .iter()
        .map(|s| (s.make)(&platform_keystrokes(s.keys), s.context))
        .collect();
    bindings.extend(
        ALIASES
            .iter()
            .map(|(keys, ctx, make)| make(&platform_keystrokes(keys), *ctx)),
    );
    // Quit is platform-specific: ⌘Q on macOS. Windows/Linux quit via the native
    // window close (Alt+F4 / the close button), already wired in `main` - so no
    // app-level binding there (Ctrl+Q is not a standard quit key).
    if cfg!(target_os = "macos") {
        bindings.push(KeyBinding::new("cmd-q", Quit, Some("App")));
    }
    cx.bind_keys(bindings);
}

/// The cheat-sheet contents: bindings grouped by [`Group`], each as a
/// `(display keystroke, label)` pair. Built from [`SHORTCUTS`] so it always
/// matches what is actually bound.
pub fn cheat_sheet() -> Vec<(&'static str, Vec<(String, &'static str)>)> {
    Group::ALL
        .iter()
        .map(|group| {
            let mut rows: Vec<(String, &'static str)> = SHORTCUTS
                .iter()
                .filter(|s| s.group == *group)
                .map(|s| (display_keys(s.keys), s.label))
                .collect();
            // Enter is handled natively by the focused/primary button (no binding
            // to list), so surface it here for discoverability.
            if *group == Group::Dialogs {
                rows.insert(0, (display_keys("enter"), "Confirm / activate"));
                rows.push((display_keys("space"), "Activate focused"));
            }
            // Quit is platform-specific and bound outside the table.
            if *group == Group::Application {
                let quit = if cfg!(target_os = "macos") {
                    display_keys("cmd-q")
                } else {
                    "Alt+F4".to_string()
                };
                rows.push((quit, "Quit"));
            }
            (group.title(), rows)
        })
        .collect()
}

/// Map the canonical keystroke (`cmd`-primary) to the running platform: `cmd`
/// stays on macOS, becomes `ctrl` elsewhere.
#[cfg(target_os = "macos")]
fn platform_keystrokes(keys: &str) -> String {
    keys.to_string()
}

#[cfg(not(target_os = "macos"))]
fn platform_keystrokes(keys: &str) -> String {
    keys.replace("cmd", "ctrl")
}

/// Pretty-print a keystroke for the cheat-sheet (⌘⇧ glyphs on macOS, `Ctrl+…`
/// elsewhere).
fn display_keys(keys: &str) -> String {
    let mac = cfg!(target_os = "macos");
    let sep = if mac { "" } else { "+" };
    keys.split_whitespace()
        .map(|chord| {
            chord
                .split('-')
                .map(|tok| token(tok, mac))
                .collect::<Vec<_>>()
                .join(sep)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn token(tok: &str, mac: bool) -> String {
    let mapped = match tok {
        "cmd" => return if mac { "⌘".into() } else { "Ctrl".into() },
        "ctrl" => return if mac { "⌃".into() } else { "Ctrl".into() },
        "shift" => return if mac { "⇧".into() } else { "Shift".into() },
        "alt" => return if mac { "⌥".into() } else { "Alt".into() },
        "enter" => "↩",
        "escape" => "Esc",
        "backspace" => "⌫",
        "delete" => "Del",
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        "home" => "Home",
        "end" => "End",
        other => {
            // Single letters read better upper-cased (n → N); punctuation as-is.
            if other.len() == 1 && other.chars().all(|c| c.is_ascii_alphabetic()) {
                return other.to_uppercase();
            }
            other
        }
    };
    mapped.to_string()
}
