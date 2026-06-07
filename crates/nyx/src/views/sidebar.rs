//! The left sidebar: header, saved connections, footer.

use gpui::{
    div, prelude::*, px, radians, Context, FontWeight, MouseButton, SharedString, Transformation,
};
use nyx_ui::{ActiveTheme, Badge, Button, ButtonSize, ButtonVariant, IconButton};

use crate::icon::icon;
use crate::state::models::{protocol_badge, ConnectionVm};
use crate::state::AppState;

use super::{status_dot, titlebar_drag};

/// Left inset of the titlebar strip. On macOS it clears the traffic lights
/// overlapping this region; on other platforms the native caption bar is
/// separate, so only normal padding is needed.
#[cfg(target_os = "macos")]
const TITLEBAR_LEFT_INSET: f32 = 72.;
#[cfg(not(target_os = "macos"))]
const TITLEBAR_LEFT_INSET: f32 = 10.;

/// Render the sidebar.
pub fn render(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let saved = state.connections.len();

    // `cx.listener` results aren't `Clone`, so each call site gets its own.
    let header_new = cx.listener(|this, _, _, cx| {
        this.open_editor_create(cx);
        cx.notify();
    });
    let footer_new = cx.listener(|this, _, _, cx| {
        this.open_editor_create(cx);
        cx.notify();
    });

    div()
        .flex()
        .flex_col()
        .min_h_0()
        .w(px(244.))
        .bg(theme.bg_panel)
        .child(
            // Header doubles as the left titlebar; left inset clears the macOS
            // traffic lights (no-op on platforms with a native caption bar).
            titlebar_drag(
                div()
                    .id("titlebar-left")
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(38.))
                    .pl(px(TITLEBAR_LEFT_INSET))
                    .pr(px(10.))
                    .flex_shrink_0()
                    .child(div().flex_1())
                    .child(
                        IconButton::new("sb-new", icon("plus", 15., theme.text_faint))
                            .on_click(header_new),
                    ),
            ),
        )
        .child(
            div()
                .id("sidebar-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pb_2()
                .child(group(
                    state,
                    "Saved",
                    saved,
                    &state.connections_all(),
                    None,
                    cx,
                )),
        )
        .child(
            div()
                .flex()
                .gap_1()
                .p_1p5()
                .border_t_1()
                .border_color(theme.border_soft)
                .child(
                    div().flex_1().child(
                        Button::new("sb-foot-new", "New")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .focusable(false)
                            .on_click(footer_new),
                    ),
                ),
        )
}

/// Render a connection group. When `collapsed` is `Some`, the header is a
/// collapse toggle; `None` renders a plain, always-expanded group.
fn group(
    state: &AppState,
    label: &'static str,
    count: usize,
    conns: &[&ConnectionVm],
    collapsed: Option<bool>,
    cx: &mut Context<AppState>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    // Row ids are namespaced by group so a profile appearing in both Recent and
    // Saved doesn't collide on a duplicate element id.
    let prefix = label;
    let is_collapsed = collapsed.unwrap_or(false);

    let mut header = div()
        .id(SharedString::from(format!("group-{label}")))
        .flex()
        .items_center()
        .gap_1p5()
        .pt_3()
        .pb_1()
        .pl(px(14.))
        .pr_3()
        .text_color(theme.text_dim);
    if collapsed.is_some() {
        // Disclosure chevron: down when expanded, right when collapsed.
        let rotation = if is_collapsed {
            -std::f32::consts::FRAC_PI_2
        } else {
            0.
        };
        header = header
            .cursor_pointer()
            .hover(|s| s.text_color(theme.text_muted))
            .child(
                icon("chevD", 12., theme.text_dim)
                    .with_transformation(Transformation::rotate(radians(rotation))),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.toggle_recent_collapsed();
                cx.notify();
            }));
    }
    header = header
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .child(label),
        )
        .child(div().text_xs().child(format!("· {count}")));

    let rows = if is_collapsed {
        Vec::new()
    } else {
        conns
            .iter()
            .map(|conn| conn_row(state, conn, prefix, cx))
            .collect::<Vec<_>>()
    };

    div().flex().flex_col().child(header).children(rows)
}

fn conn_row(
    state: &AppState,
    conn: &ConnectionVm,
    prefix: &str,
    cx: &mut Context<AppState>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let id = conn.profile.id.clone();
    let is_active = state.active_id.as_deref() == Some(id.as_str());
    let is_online = state.online_id.as_deref() == Some(id.as_str());
    let (badge_variant, badge_label) = protocol_badge(conn.profile.protocol);

    let dot_color = if is_online {
        theme.green
    } else {
        theme.text_dim
    };
    let ring = is_online.then(|| theme.green.opacity(0.16));

    let open_id = id.clone();
    let menu_id = id.clone();
    let menu_name = gpui::SharedString::from(conn.profile.name.clone());

    div()
        .id(gpui::SharedString::from(format!("{prefix}-conn-{id}")))
        .relative()
        .flex()
        .items_center()
        .gap(px(9.))
        .py(px(5.))
        .pl(px(14.))
        .pr_3()
        .cursor_pointer()
        .when(is_active, |this| this.bg(theme.bg_active))
        .when(!is_active, |this| this.hover(|s| s.bg(theme.bg_hover)))
        .child(status_dot(dot_color, ring))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.text)
                        .truncate()
                        .child(conn.profile.name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.text_faint)
                        .font_family(crate::assets::FONT_MONO)
                        .truncate()
                        .child(conn.user_host()),
                ),
        )
        .child(Badge::new(badge_label).variant(badge_variant))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_connection(&open_id, cx);
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                this.open_row_menu(menu_id.clone(), menu_name.clone(), event.position);
                cx.notify();
            }),
        )
}
