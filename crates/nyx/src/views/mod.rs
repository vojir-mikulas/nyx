// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The view tree. Each module is a `RenderOnce`-style helper that reads a
//! `&AppState` and emits elements; mutation happens through `cx.listener`
//! handlers that call back into [`crate::state::AppState`].

pub mod browser;
pub mod sidebar;
pub mod status_bar;
pub mod transfer_dock;
pub mod welcome;

use gpui::{div, prelude::*, px, Hsla};

/// A small themed status dot (online/offline, connection state). App-local by
/// design (M1 gap G5): a 6–7px circle is not worth a component, but it always
/// uses a theme token, never a raw color.
pub fn status_dot(color: Hsla, ring: Option<Hsla>) -> impl IntoElement {
    div()
        .size(px(7.))
        .rounded_full()
        .bg(color)
        .flex_shrink_0()
        .when_some(ring, |this, ring| {
            this.shadow(vec![gpui::BoxShadow {
                color: ring,
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(3.),
                inset: false,
            }])
        })
}
