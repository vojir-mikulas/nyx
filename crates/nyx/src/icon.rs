//! App-side icon helper.
//!
//! `nyx-ui` stays icon-agnostic so the Flint extraction carries no asset
//! opinions, so the concrete icon set lives in the app. Icons are embedded SVGs
//! (see [`crate::assets`]) rendered via GPUI's `svg()`.

use std::time::Duration;

use gpui::{
    percentage, prelude::*, px, svg, Animation, AnimationExt, ElementId, Hsla, Svg, Transformation,
};

/// An icon glyph (`name` is the file stem under `assets/icons/`) at square
/// `size` px, tinted with `color`. The color must be set here: GPUI's `svg()`
/// does not inherit `text_color` from ancestors, so an uncolored icon paints nothing.
pub fn icon(name: &str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
}

/// A continuously-rotating ring spinner — the app-wide loading indicator. `id`
/// must be unique among siblings so each spinner keeps its own animation state.
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    icon("spinner", size, color).with_animation(
        id,
        Animation::new(Duration::from_secs(1)).repeat(),
        |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
    )
}
