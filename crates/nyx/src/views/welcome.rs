//! The welcome / connection-manager screen (shown when nothing is open).

use flint::{ActiveTheme, Badge, IconButton, IconButtonSize};
use gpui::{actions, div, prelude::*, px, svg, Context, FontWeight};
use nyx_core::Protocol;

use crate::icon::icon;
use crate::state::models::{protocol_badge, ConnectionVm};
use crate::state::AppState;

actions!(
    nyx_welcome,
    [
        /// Activate the focused welcome-list row (open the connection, or create
        /// a new one for the New button).
        ActivateRow,
    ]
);

/// Render the welcome screen.
pub fn render(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();

    let cards = state
        .connections
        .iter()
        .map(|conn| card(state, conn, cx))
        .collect::<Vec<_>>();
    let recents = state
        .recent_connections()
        .into_iter()
        .take(3)
        .map(|conn| recent_row(state, conn, cx))
        .collect::<Vec<_>>();

    div()
        .id("welcome")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .bg(theme.bg_app)
        .text_color(theme.text)
        .child(
            div()
                .w_full()
                .max_w(px(620.))
                .px_8()
                .pt(px(72.))
                .pb(px(60.))
                .child(logo(cx))
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .mt_3()
                        .child("Welcome to Nyx"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.text_faint)
                        .mt_1()
                        .child("A fast, reliable file transfer client. Pick a server to connect, or create a new profile."),
                )
                .child(section_label("Saved connections", cx))
                .child(div().flex().flex_col().gap_1p5().children(cards))
                .child(new_button(state, cx))
                .when(!recents.is_empty(), |this| {
                    this.child(section_label("Recent", cx))
                        .child(div().flex().flex_col().gap_1p5().children(recents))
                }),
        )
}

fn logo(cx: &Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    // GPUI's `svg()` ignores the file's fills and tints the whole glyph with
    // `text_color`, so the mark inherits the current accent.
    svg()
        .path("nyx_black.svg")
        .size(px(40.))
        .text_color(theme.accent)
        .flex_shrink_0()
}

fn section_label(label: &'static str, cx: &Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .flex()
        .items_center()
        .gap_2()
        .mt(px(26.))
        .mb_2()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_dim)
                .child(label),
        )
        .child(div().flex_1().h(px(1.)).bg(theme.border_soft))
}

fn card(state: &AppState, conn: &ConnectionVm, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let id = conn.profile.id.clone();
    // Out of the tab ring while a modal is open, so focus stays trapped in it.
    let focus = (!state.has_overlay())
        .then(|| state.row_focus(&format!("card:{id}")))
        .flatten();
    let activate_id = id.clone();
    let (badge_variant, badge_label) = protocol_badge(conn.profile.protocol);
    let glyph = if conn.profile.protocol == Protocol::Sftp {
        "server"
    } else {
        "globe"
    };
    // `user@host:port`, plus `· path` only when a remote path is set.
    let subtitle = match conn.profile.remote_path.as_deref() {
        Some(path) if !path.trim().is_empty() => {
            format!("{}  ·  {}", conn.user_host_port(), path)
        }
        _ => conn.user_host_port(),
    };

    let accent = conn.color.color(&theme);

    let name: gpui::SharedString = conn.profile.name.clone().into();
    let group = gpui::SharedString::from(format!("wm-card-{id}"));
    let open_id = id.clone();
    let menu_id = id.clone();
    let menu_name = name.clone();
    let edit_id = id.clone();
    let remove_id = id.clone();
    let remove_name = name.clone();

    div()
        .id(gpui::SharedString::from(format!("wm-{id}")))
        .group(group.clone())
        .flex()
        .items_center()
        .gap(px(13.))
        .p(px(12.))
        .px(px(14.))
        .rounded(theme.radius)
        .bg(theme.bg_elevated)
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|s| s.border_color(theme.border_strong).bg(theme.bg_active))
        .when_some(focus, |this, handle| {
            this.track_focus(&handle)
                .key_context("ConnRow")
                .focus(|s| s.border_color(theme.accent))
                .on_action(cx.listener(move |this, _: &ActivateRow, _, cx| {
                    this.open_connection(&activate_id, cx);
                    cx.notify();
                }))
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(34.))
                .rounded(px(8.))
                .bg(accent.opacity(0.12))
                .border_1()
                .border_color(accent.opacity(0.35))
                .text_color(accent)
                .child(icon(glyph, 17., accent)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(conn.profile.name.clone()),
                        )
                        .child(Badge::new(badge_label).variant(badge_variant)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.text_faint)
                        .font_family(crate::assets::FONT_MONO)
                        .truncate()
                        .child(subtitle),
                ),
        )
        // Chevron by default, swapped for Edit / Remove on hover (the buttons
        // stop propagation so they don't open the connection).
        .child(
            div()
                .relative()
                .flex()
                .items_center()
                .justify_end()
                .w(px(58.))
                .child(
                    div()
                        .group_hover(group.clone(), |s| s.invisible())
                        .text_color(theme.text_dim)
                        .child(icon("chevR", 16., theme.text_dim)),
                )
                .child(
                    div()
                        .absolute()
                        .right_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .invisible()
                        .group_hover(group, |s| s.visible())
                        .child(
                            IconButton::new(
                                gpui::SharedString::from(format!("wm-edit-{edit_id}")),
                                icon("pencil", 14., theme.text_muted),
                            )
                            .size(IconButtonSize::Xs)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.open_editor_edit(&edit_id, cx);
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            IconButton::new(
                                gpui::SharedString::from(format!("wm-remove-{remove_id}")),
                                icon("trash", 14., theme.red),
                            )
                            .size(IconButtonSize::Xs)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.open_delete_confirm(
                                        remove_id.clone(),
                                        remove_name.clone(),
                                    );
                                    cx.notify();
                                },
                            )),
                        ),
                ),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_connection(&open_id, cx);
            cx.notify();
        }))
        .on_mouse_down(
            gpui::MouseButton::Right,
            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                this.open_row_menu(menu_id.clone(), menu_name.clone(), event.position);
                cx.notify();
            }),
        )
}

fn recent_row(
    state: &AppState,
    conn: &ConnectionVm,
    cx: &mut Context<AppState>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let id = conn.profile.id.clone();
    let focus = (!state.has_overlay())
        .then(|| state.row_focus(&format!("recent:{id}")))
        .flatten();
    let activate_id = id.clone();
    div()
        .id(gpui::SharedString::from(format!("wm-rc-{id}")))
        .flex()
        .items_center()
        .gap(px(13.))
        .py(px(9.))
        .px(px(14.))
        .rounded(theme.radius)
        .bg(theme.bg_elevated)
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|s| s.border_color(theme.border_strong).bg(theme.bg_active))
        .when_some(focus, |this, handle| {
            this.track_focus(&handle)
                .key_context("ConnRow")
                .focus(|s| s.border_color(theme.accent))
                .on_action(cx.listener(move |this, _: &ActivateRow, _, cx| {
                    this.open_connection(&activate_id, cx);
                    cx.notify();
                }))
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(28.))
                .rounded(px(8.))
                .bg(theme.bg_input)
                .border_1()
                .border_color(theme.border_soft)
                .text_color(theme.text_faint)
                .child(icon("clock", 14., theme.text_faint)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().truncate().child(conn.profile.name.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.text_faint)
                        .child(conn.last_used.clone().unwrap_or_else(|| "—".into())),
                ),
        )
        .child(
            div()
                .text_color(theme.text_dim)
                .child(icon("chevR", 15., theme.text_dim)),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_connection(&id, cx);
            cx.notify();
        }))
}

fn new_button(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let focus = (!state.has_overlay())
        .then(|| state.row_focus("new"))
        .flatten();
    div()
        .id("wm-new")
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .w_full()
        .h(px(42.))
        .mt_2()
        .rounded(theme.radius)
        .border_1()
        .border_dashed()
        .border_color(theme.border_strong)
        .text_sm()
        .text_color(theme.text_muted)
        .cursor_pointer()
        .hover(|s| {
            s.border_color(theme.accent)
                .text_color(theme.text)
                .bg(theme.accent_ghost)
        })
        .when_some(focus, |this, handle| {
            this.track_focus(&handle)
                .key_context("ConnRow")
                .focus(|s| s.border_color(theme.accent))
                .on_action(cx.listener(|this, _: &ActivateRow, _, cx| {
                    this.open_editor_create(cx);
                    cx.notify();
                }))
        })
        .child(icon("plus", 15., theme.text_muted))
        .child("New connection")
        .child(
            div()
                .ml_1()
                .px_1p5()
                .rounded(px(4.))
                .text_xs()
                .text_color(theme.text_faint)
                .bg(theme.bg_input)
                .border_1()
                .border_color(theme.border)
                .font_family(crate::assets::FONT_MONO)
                .child("⌘N"),
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.open_editor_create(cx);
            cx.notify();
        }))
}
