// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The bottom status bar: connection state, host, transfer speed, counts.

use gpui::{div, prelude::*, px, Context};
use nyx_ui::ActiveTheme;

use crate::icon::icon;
use crate::state::models::{fmt_size, protocol_badge};
use crate::state::{AppState, View};

use super::status_dot;

/// Render the status bar.
pub fn render(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let mono = crate::assets::FONT_MONO;

    let base = div()
        .flex()
        .items_center()
        .h(px(22.))
        .flex_shrink_0()
        .px_1()
        .bg(theme.bg_panel)
        .border_t_1()
        .border_color(theme.border)
        .text_color(theme.text_faint);

    let toggle_settings = cx.listener(|this, _, _, cx| {
        this.tweaks_open = true;
        cx.notify();
    });

    if state.view == View::Browse {
        if let Some(conn) = state.active_conn() {
            let online = state.online_id.is_some();
            let (_, proto_label) = protocol_badge(conn.profile.protocol);
            let (active_count, speed) = state.active_speed();
            let counts = if state.selected_count() > 0 {
                format!("{} selected", state.selected_count())
            } else {
                format!("{} items", state.item_count())
            };

            return base
                .child(
                    item(cx)
                        .child(status_dot(
                            if online { theme.green } else { theme.text_dim },
                            None,
                        ))
                        .child(if online { "Connected" } else { "Disconnected" }),
                )
                .child(item(cx).font_family(mono).child(proto_label))
                .child(
                    item(cx)
                        .id("sb-host")
                        .font_family(mono)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.bg_hover).text_color(theme.text_muted))
                        .child(conn.user_host_port()),
                )
                .child(div().flex_1())
                .when(active_count > 0, |this| {
                    this.child(
                        item(cx)
                            .font_family(mono)
                            .text_color(theme.green)
                            .child(icon("zap", 11., theme.green))
                            .child(format!("{}/s · {active_count} active", fmt_size(speed))),
                    )
                })
                .child(item(cx).font_family(mono).child(counts))
                .child(
                    item(cx)
                        .id("sb-dock")
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.bg_hover).text_color(theme.text_muted))
                        .child(icon("panelBottom", 12., theme.text_faint))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.dock_open = !this.dock_open;
                            cx.notify();
                        })),
                )
                .child(
                    item(cx)
                        .id("sb-settings")
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.bg_hover).text_color(theme.text_muted))
                        .child(icon("settings", 12., theme.text_faint))
                        .on_click(toggle_settings),
                );
        }
    }

    base.child(
        item(cx)
            .child(status_dot(theme.text_dim, None))
            .child("No connection"),
    )
    .child(div().flex_1())
    .child(item(cx).font_family(mono).child("Nyx 0.1.0"))
    .child(
        item(cx)
            .id("sb-settings")
            .cursor_pointer()
            .hover(|s| s.bg(theme.bg_hover).text_color(theme.text_muted))
            .child(icon("settings", 12., theme.text_faint))
            .on_click(toggle_settings),
    )
}

fn item(_cx: &Context<AppState>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .h_full()
        .text_xs()
        .whitespace_nowrap()
}
