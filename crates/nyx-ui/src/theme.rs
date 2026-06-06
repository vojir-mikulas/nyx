//! The semantic theme layer: the [`Theme`] token set, installed as a GPUI
//! [`Global`], and the [`ActiveTheme`] accessor.
//!
//! Tokens are **semantic and generic** (`bg_panel`, `accent`) — never
//! app-specific (`sftp_badge_color`). App-specific styling lives in the app, not
//! here. The concrete One Dark / GitHub Dark values are in `tokens.rs`.

use gpui::{App, Global, Hsla, Pixels};

/// A complete set of design tokens for one theme.
///
/// Stored as a GPUI [`Global`]; read it in `render` via [`ActiveTheme::theme`]
/// (e.g. `cx.theme().bg_app`). Ported from `design/styles.css`.
#[derive(Clone, Debug)]
pub struct Theme {
    /// Human-readable theme name (e.g. `"One Dark"`).
    pub name: &'static str,

    // --- Surfaces ---
    /// Main / editor surface.
    pub bg_app: Hsla,
    /// Sidebar + bottom dock (a touch darker).
    pub bg_panel: Hsla,
    /// Deepest surface (status bar, empty states).
    pub bg_panel_2: Hsla,
    /// Modals, popovers, active tab.
    pub bg_elevated: Hsla,
    /// Toolbars / tab strip.
    pub bg_bar: Hsla,
    /// Hover background.
    pub bg_hover: Hsla,
    /// Hovered/selected neutral background.
    pub bg_active: Hsla,
    /// Blue-tinted selection background.
    pub bg_selected: Hsla,
    /// Text input background.
    pub bg_input: Hsla,

    // --- Borders ---
    /// Default border.
    pub border: Hsla,
    /// Subtle/soft border.
    pub border_soft: Hsla,
    /// Strong/emphasized border.
    pub border_strong: Hsla,

    // --- Text ---
    /// Primary text.
    pub text: Hsla,
    /// Muted/secondary text.
    pub text_muted: Hsla,
    /// Faint text.
    pub text_faint: Hsla,
    /// Dimmest text (labels, disabled).
    pub text_dim: Hsla,

    // --- Accent ---
    /// Primary accent.
    pub accent: Hsla,
    /// Accent hover state.
    pub accent_hover: Hsla,
    /// Translucent accent — focus-ring glow, ghost-accent surfaces.
    pub accent_ghost: Hsla,
    /// Foreground used on top of `accent`.
    pub on_accent: Hsla,

    // --- Status / syntax palette ---
    /// Green (success, FTPS).
    pub green: Hsla,
    /// Red (error, danger).
    pub red: Hsla,
    /// Blue (info, running, FTP, folders).
    pub blue: Hsla,
    /// Purple (SFTP).
    pub purple: Hsla,
    /// Yellow (warning).
    pub yellow: Hsla,
    /// Orange (archives, secondary warning).
    pub orange: Hsla,

    // --- Metrics ---
    /// File-row height.
    pub row_height: Pixels,
    /// Default corner radius.
    pub radius: Pixels,
    /// Small corner radius (chips, icon buttons, menu items).
    pub radius_sm: Pixels,
}

impl Global for Theme {}

/// Accessor for the active [`Theme`] global.
///
/// Implemented for [`App`], so it is reachable from `render` via deref
/// (`cx.theme()` on a `&mut Context<_>` or `&mut App`).
pub trait ActiveTheme {
    /// The currently installed theme.
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}
