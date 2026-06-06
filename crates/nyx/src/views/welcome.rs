// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The welcome / connection-manager screen (shown when nothing is open).

use gpui::{div, prelude::*, px, Context, FontWeight};
use nyx_core::Protocol;
use nyx_ui::{ActiveTheme, Badge, ToastVariant};

use crate::icon::icon;
use crate::state::models::{protocol_badge, ConnectionVm};
use crate::state::AppState;

/// Render the welcome screen.
pub fn render(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();

    let cards = state
        .connections
        .iter()
        .map(|conn| card(conn, cx))
        .collect::<Vec<_>>();
    let recents = state
        .connections
        .iter()
        .filter(|c| c.is_recent)
        .take(3)
        .map(|conn| recent_row(conn, cx))
        .collect::<Vec<_>>();

    let new_conn = cx.listener(|this, _, _, cx| {
        this.push_toast("New connection — coming in M3", ToastVariant::Info, cx);
        cx.notify();
    });

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
                .child(new_button(new_conn, cx))
                .when(!recents.is_empty(), |this| {
                    this.child(section_label("Recent", cx))
                        .child(div().flex().flex_col().gap_1p5().children(recents))
                }),
        )
}

fn logo(cx: &Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .flex()
        .items_center()
        .justify_center()
        .size(px(40.))
        .rounded(px(10.))
        .bg(theme.accent)
        .text_color(theme.on_accent)
        .child(icon("zap", 22.))
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
        // Divider line (M1 gap G4: composed app-locally with a token, no hex).
        .child(div().flex_1().h(px(1.)).bg(theme.border_soft))
}

fn card(conn: &ConnectionVm, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let id = conn.profile.id.clone();
    let (badge_variant, badge_label) = protocol_badge(conn.profile.protocol);
    let glyph = if conn.profile.protocol == Protocol::Sftp {
        "server"
    } else {
        "globe"
    };
    let path = conn.profile.remote_path.clone().unwrap_or_default();

    div()
        .id(gpui::SharedString::from(format!("wm-{id}")))
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
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(34.))
                .rounded(px(8.))
                .bg(theme.bg_input)
                .border_1()
                .border_color(theme.border_soft)
                .text_color(conn.color.color(&theme))
                .child(icon(glyph, 17.)),
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
                        .child(format!("{}  ·  {}", conn.user_host_port(), path)),
                ),
        )
        .child(div().text_color(theme.text_dim).child(icon("chevR", 16.)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_connection(&id, cx);
            cx.notify();
        }))
}

fn recent_row(conn: &ConnectionVm, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let id = conn.profile.id.clone();
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
                .child(icon("clock", 14.)),
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
        .child(div().text_color(theme.text_dim).child(icon("chevR", 15.)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_connection(&id, cx);
            cx.notify();
        }))
}

fn new_button(
    handler: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    cx: &Context<AppState>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
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
        .child(icon("plus", 15.))
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
        .on_click(handler)
}
