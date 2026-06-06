// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! `Badge` — a small, colored label chip (status, protocol tags, counts).
//!
//! Variants are **semantic** ([`BadgeVariant`]), not app-specific: the app maps
//! its own concepts (e.g. an SFTP protocol) onto a generic variant. Each colored
//! variant renders as the design's translucent-fill / solid-text chip; the fill
//! is derived from the text color at low opacity, so it tracks the active theme.

use gpui::{div, prelude::*, App, Hsla, SharedString, Window};

use crate::theme::ActiveTheme;

/// Semantic color of a [`Badge`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BadgeVariant {
    /// Neutral grey (default).
    #[default]
    Neutral,
    /// Accent-colored.
    Accent,
    /// Success / positive (green).
    Success,
    /// Danger / error (red).
    Danger,
    /// Informational (blue).
    Info,
    /// Warning (yellow).
    Warning,
    /// Distinct / special (purple).
    Special,
}

/// A small colored label chip.
///
/// ```ignore
/// Badge::new("SFTP").variant(BadgeVariant::Special);
/// ```
#[derive(IntoElement)]
pub struct Badge {
    label: SharedString,
    variant: BadgeVariant,
}

impl Badge {
    /// Create a badge with the given `label`.
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            variant: BadgeVariant::default(),
        }
    }

    /// Set the color variant.
    pub fn variant(mut self, variant: BadgeVariant) -> Self {
        self.variant = variant;
        self
    }
}

/// Resolve `(foreground, background)` for a variant against the active theme.
fn variant_colors(variant: BadgeVariant, theme: &crate::Theme) -> (Hsla, Hsla) {
    let tinted = |c: Hsla| (c, c.opacity(0.13));
    match variant {
        BadgeVariant::Neutral => (theme.text_muted, theme.bg_active),
        BadgeVariant::Accent => tinted(theme.accent),
        BadgeVariant::Success => tinted(theme.green),
        BadgeVariant::Danger => tinted(theme.red),
        BadgeVariant::Info => tinted(theme.blue),
        BadgeVariant::Warning => tinted(theme.yellow),
        BadgeVariant::Special => tinted(theme.purple),
    }
}

impl RenderOnce for Badge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, bg) = variant_colors(self.variant, theme);

        div()
            .flex()
            .items_center()
            .px_1p5()
            .py(gpui::px(1.))
            .rounded(theme.radius_sm)
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(fg)
            .bg(bg)
            .child(self.label)
    }
}
