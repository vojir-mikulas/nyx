// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! `Toast` — a small, transient notification surface.
//!
//! The component renders a single toast (elevated pill with a status dot and a
//! message). Stacking, positioning (bottom-right in our design) and auto-dismiss
//! timing are the caller's concern — `Toast` is just the visual.
//!
//! ```ignore
//! Toast::new("Uploaded 3 files").variant(ToastVariant::Success)
//! ```

use gpui::{div, prelude::*, App, Hsla, SharedString, Window};

use crate::theme::ActiveTheme;

/// Semantic kind of a [`Toast`], driving the status-dot color.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ToastVariant {
    /// Neutral / informational accent (default).
    #[default]
    Info,
    /// Success (green).
    Success,
    /// Error (red).
    Error,
}

/// A small notification surface.
#[derive(IntoElement)]
pub struct Toast {
    message: SharedString,
    variant: ToastVariant,
}

impl Toast {
    /// Create a toast with the given `message`.
    pub fn new(message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
            variant: ToastVariant::default(),
        }
    }

    /// Set the variant (status color).
    pub fn variant(mut self, variant: ToastVariant) -> Self {
        self.variant = variant;
        self
    }
}

fn dot_color(variant: ToastVariant, theme: &crate::Theme) -> Hsla {
    match variant {
        ToastVariant::Info => theme.accent,
        ToastVariant::Success => theme.green,
        ToastVariant::Error => theme.red,
    }
}

impl RenderOnce for Toast {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let dot = dot_color(self.variant, theme);

        div()
            .flex()
            .items_center()
            .gap_2p5()
            .min_w(gpui::px(220.))
            .px_3()
            .py_2p5()
            .bg(theme.bg_elevated)
            .border_1()
            .border_color(theme.border_strong)
            .rounded(gpui::px(7.))
            .shadow_lg()
            .text_sm()
            .text_color(theme.text)
            .child(
                div()
                    .size(gpui::px(8.))
                    .rounded_full()
                    .bg(dot)
                    .flex_shrink_0(),
            )
            .child(self.message)
    }
}
