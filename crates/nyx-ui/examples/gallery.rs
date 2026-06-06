// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The component gallery — `nyx-ui`'s "storybook".
//!
//! Run with `cargo run -p nyx-ui --example gallery`. It installs a theme global
//! and renders every component in its key states. A click on the header toggles
//! One Dark ↔ GitHub Dark so theming is verifiable at a glance. More components
//! are added here first as they land (see plan-02).

use gpui::{
    div, prelude::*, App, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use nyx_ui::prelude::*;

struct Gallery;

impl Gallery {
    fn section(&self, title: impl Into<SharedString>, cx: &App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().text_dim)
                    .child(title.into()),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(Button::new("primary", "Primary").variant(ButtonVariant::Primary))
                    .child(Button::new("secondary", "Secondary").variant(ButtonVariant::Secondary))
                    .child(Button::new("ghost", "Ghost").variant(ButtonVariant::Ghost))
                    .child(Button::new("danger", "Danger").variant(ButtonVariant::Danger))
                    .child(
                        Button::new("disabled", "Disabled")
                            .variant(ButtonVariant::Primary)
                            .disabled(true),
                    )
                    .child(
                        Button::new("small", "Small")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm),
                    ),
            )
    }
}

impl Render for Gallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_name = cx.theme().name;

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_6()
            .bg(cx.theme().bg_app)
            .text_color(cx.theme().text)
            .p_8()
            .child(
                div()
                    .id("theme-toggle")
                    .cursor_pointer()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xl().child("nyx-ui gallery"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().text_muted)
                            .child(format!("Theme: {theme_name}  (click to toggle)")),
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        let next = if cx.theme().name == "One Dark" {
                            Theme::github_dark()
                        } else {
                            Theme::one_dark()
                        };
                        cx.set_global(next);
                        cx.notify();
                    })),
            )
            .child(self.section("Buttons", cx))
    }
}

fn main() {
    application().run(|cx: &mut App| {
        cx.set_global(Theme::one_dark());
        let bounds = Bounds::centered(None, gpui::size(gpui::px(900.0), gpui::px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Gallery),
        )
        .expect("failed to open gallery window");
        cx.activate(true);
    });
}
