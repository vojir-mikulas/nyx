// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The root view: the app-shell grid, view routing, and global overlays
//! (tweaks modal + toasts). [`AppState`] is the single root entity; this file
//! is its `Render` impl.

use gpui::{div, prelude::*, px, Context, FontWeight, Window};
use nyx_ui::{ActiveTheme, Button, ButtonVariant, Modal, Segmented, Theme, Toast, Toggle};

use crate::assets::FONT_UI;
use crate::state::models::Density;
use crate::state::{AppState, View};
use crate::views;

impl Render for AppState {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();

        let sidebar = self.sidebar_open.then(|| views::sidebar::render(self, cx));

        let body: gpui::AnyElement = match self.view {
            View::Welcome => views::welcome::render(self, cx).into_any_element(),
            View::Browse => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(views::browser::render(self, cx))
                .child(views::transfer_dock::render(self, cx))
                .into_any_element(),
        };

        let main_col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .bg(theme.bg_app)
            .border_l_1()
            .border_color(theme.border_soft)
            .child(body);

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .font_family(FONT_UI)
            .bg(theme.bg_panel_2)
            .text_color(theme.text)
            .text_sm()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when_some(sidebar, |this, sidebar| this.child(sidebar))
                    .child(main_col),
            )
            .child(views::status_bar::render(self, cx))
            .when(self.tweaks_open, |this| {
                let modal = tweaks_modal(self, cx);
                this.child(modal)
            })
            .when_some(self.toast.as_ref(), |this, toast| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_end()
                        .justify_end()
                        .p_4()
                        .child(Toast::new(toast.message.clone()).variant(toast.variant)),
                )
            })
    }
}

/// The in-memory tweaks modal (density, permissions column, theme).
fn tweaks_modal(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let density_ix = state.density.index();
    let show_perms = state.show_perms;
    let theme_ix = if cx.theme().name == "One Dark" { 0 } else { 1 };
    let view = cx.entity();

    Modal::new("tweaks")
        .title("Tweaks")
        .width(px(420.))
        .on_close({
            let view = view.clone();
            move |_window, cx| {
                view.update(cx, |this, cx| {
                    this.tweaks_open = false;
                    cx.notify();
                });
            }
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(field(
                    "Color scheme",
                    Segmented::new("tw-theme")
                        .segment("One Dark")
                        .segment("GitHub Dark")
                        .selected(theme_ix)
                        .on_select({
                            let view = view.clone();
                            move |ix, _window, cx| {
                                let next = if ix == 0 {
                                    Theme::one_dark()
                                } else {
                                    Theme::github_dark()
                                };
                                cx.set_global(next);
                                view.update(cx, |_, cx| cx.notify());
                            }
                        }),
                    cx,
                ))
                .child(field(
                    "Row density",
                    Segmented::new("tw-density")
                        .segment("Compact")
                        .segment("Comfortable")
                        .segment("Spacious")
                        .selected(density_ix)
                        .on_select({
                            let view = view.clone();
                            move |ix, _window, cx| {
                                view.update(cx, |this, cx| {
                                    this.density = Density::ALL[ix.min(2)];
                                    cx.notify();
                                });
                            }
                        }),
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.text_muted)
                                .child("Permissions column"),
                        )
                        .child(Toggle::new("tw-perms", show_perms).on_change({
                            let view = view.clone();
                            move |on, _window, cx| {
                                let on = *on;
                                view.update(cx, |this, cx| {
                                    this.show_perms = on;
                                    cx.notify();
                                });
                            }
                        })),
                ),
        )
        .footer(
            div().flex().w_full().justify_end().child(
                Button::new("tw-done", "Done")
                    .variant(ButtonVariant::Primary)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tweaks_open = false;
                        cx.notify();
                    })),
            ),
        )
}

fn field(
    label: &'static str,
    control: impl IntoElement,
    cx: &Context<AppState>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().text_muted)
                .child(label),
        )
        .child(control)
}
