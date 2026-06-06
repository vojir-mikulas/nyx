// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The Nyx application binary.
//!
//! For now this is "hello window": it installs the One Dark theme global and
//! opens an empty, themed GPUI window. The real app shell (sidebar, browser,
//! transfer dock) is built out in later plans on top of `nyx-ui` components.

use gpui::{
    div, prelude::*, App, Bounds, Context, TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use nyx_ui::prelude::*;

/// The root view. Currently just paints the app background with the active theme.
struct Nyx;

impl Render for Nyx {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(cx.theme().bg_app)
    }
}

fn main() {
    application().run(|cx: &mut App| {
        cx.set_global(Theme::one_dark());

        let bounds = Bounds::centered(None, gpui::size(gpui::px(1000.0), gpui::px(680.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Nyx".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Nyx),
        )
        .expect("failed to open Nyx window");

        cx.activate(true);
    });
}
