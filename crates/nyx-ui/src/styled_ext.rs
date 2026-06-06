// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! `StyledExt` — the "@apply" layer: reusable, theme-aware style recipes on top
//! of GPUI's [`Styled`] builder.
//!
//! These compose common token combinations so views read declaratively
//! (`div().panel(cx)`) instead of repeating `bg`/`border` token lookups.

use gpui::{App, Styled};

use crate::theme::ActiveTheme;

/// Theme-aware style recipes, available on every [`Styled`] element.
pub trait StyledExt: Styled + Sized {
    /// Panel surface: panel background + a default border.
    fn panel(self, cx: &App) -> Self {
        self.bg(cx.theme().bg_panel)
            .border_1()
            .border_color(cx.theme().border)
    }

    /// Elevated surface (modals, popovers): elevated background + strong border.
    fn elevated(self, cx: &App) -> Self {
        self.bg(cx.theme().bg_elevated)
            .border_1()
            .border_color(cx.theme().border_strong)
    }

    /// Constrain to the standard file-row height.
    fn row_h(self, cx: &App) -> Self {
        self.h(cx.theme().row_height)
    }

    /// Accent focus ring (accent-colored border).
    fn focus_ring(self, cx: &App) -> Self {
        self.border_1().border_color(cx.theme().accent)
    }
}

impl<T: Styled> StyledExt for T {}
