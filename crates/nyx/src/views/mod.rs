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

/// Mark a top strip as the window's drag region. This registers it with the
/// platform's native hit-test (`WindowControlArea::Drag`), so macOS handles
/// dragging, edge-snapping/tiling and double-click-to-zoom itself — and respects
/// the user's "double-click a window's title bar to…" setting. Interactive
/// children (buttons) keep their own hitboxes and stay clickable.
pub fn titlebar_drag<E: InteractiveElement>(el: E) -> E {
    el.window_control_area(WindowControlArea::Drag)
}

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
