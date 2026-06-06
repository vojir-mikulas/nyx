// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! App-side icon helper.
//!
//! `nyx-ui` is intentionally icon-agnostic (it stays icon-provider-free so the
//! future Flint extraction carries no asset opinions — see plan-02 / M1 gap G7),
//! so the concrete icon set lives in the **app**. Icons are embedded SVGs (see
//! [`crate::assets`]) rendered via GPUI's `svg()`, which tints the glyph with the
//! element's `text_color` — so an icon inherits its parent's color by default and
//! can be overridden with `.text_color(..)`.

use gpui::{prelude::*, px, svg, Hsla, Svg};

/// An icon glyph at the given square `size` (px), tinted with `color`.
///
/// `name` is the file stem under `assets/icons/` (e.g. `"refresh"`). The color
/// **must** be set here: GPUI's `svg()` reads its own `text_color` and does not
/// inherit it from ancestors, so an uncolored icon paints nothing.
pub fn icon(name: &str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
}
