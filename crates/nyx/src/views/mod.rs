//! The view tree. Each module is a `RenderOnce`-style helper that reads a
//! `&AppState` and emits elements; mutation happens through `cx.listener`
//! handlers that call back into [`crate::state::AppState`].

pub mod browser;
pub mod connection_editor;
pub mod sidebar;
pub mod status_bar;
pub mod transfer_dock;
pub mod welcome;

use gpui::{div, prelude::*, px, Hsla, InteractiveElement, WindowControlArea};

/// Register a top strip as the window's native drag region, so macOS handles
/// dragging, snapping and double-click-to-zoom itself. Interactive children keep
/// their own hitboxes.
pub fn titlebar_drag<E: InteractiveElement>(el: E) -> E {
    el.window_control_area(WindowControlArea::Drag)
}

/// A small themed status dot (online/offline). App-local: too small to be worth
/// a component, but always uses a theme token, never a raw color.
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
