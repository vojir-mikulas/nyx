//! App-side icon helper.
//!
//! `nyx-ui` is intentionally icon-agnostic (it stays icon-provider-free so the
//! future Flint extraction carries no asset opinions — see plan-02 / M1 gap G7),
//! so the concrete icon set lives in the **app**. Icons are embedded SVGs (see
//! [`crate::assets`]) rendered via GPUI's `svg()`, which tints the glyph with the
//! element's `text_color` — so an icon inherits its parent's color by default and
//! can be overridden with `.text_color(..)`.

use std::time::Duration;

use gpui::{
    percentage, prelude::*, px, svg, Animation, AnimationExt, ElementId, Hsla, Svg, Transformation,
};

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

/// A minimal, continuously-rotating ring spinner — the single loading indicator
/// used app-wide (connecting overlay, directory loading, test probe). `id` must
/// be unique among sibling elements so each spinner keeps its own animation
/// state.
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    icon("spinner", size, color).with_animation(
        id,
        Animation::new(Duration::from_secs(1)).repeat(),
        |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
    )
}
