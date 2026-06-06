// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The Nyx application binary.
//!
//! M1 assembles the full app shell — sidebar, welcome / connection manager,
//! file browser, transfer dock and status bar — driven entirely by in-memory
//! fixtures (no backend, no network, no disk beyond the embedded font/icon
//! assets). The single root [`AppState`] entity owns all state and interaction
//! logic; see `docs/plans/mvp-m1-app-shell.md`.

mod app;
mod assets;
mod icon;
mod state;
mod views;

use gpui::{prelude::*, App, Bounds, TitlebarOptions, WindowBounds, WindowOptions};
use gpui_platform::application;
use nyx_ui::prelude::*;

use crate::assets::Assets;
use crate::state::AppState;

fn main() {
    init_tracing();
    application().with_assets(Assets).run(|cx: &mut App| {
        cx.set_global(Theme::one_dark());
        if let Err(err) = Assets::load_fonts(cx) {
            eprintln!("warning: failed to load vendored fonts: {err}");
        }
        TextInput::bind_keys(cx);

        let bounds = Bounds::centered(None, gpui::size(gpui::px(1100.0), gpui::px(720.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // Seamless titlebar: hide the native bar and let our chrome reach
                // the window's top edge, with the macOS traffic lights inset into
                // the top-left strip (drag/zoom wired in `views::titlebar_drag`).
                titlebar: Some(TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    traffic_light_position: Some(gpui::point(gpui::px(13.), gpui::px(13.))),
                }),
                ..Default::default()
            },
            |_, cx| cx.new(AppState::new),
        )
        .expect("failed to open Nyx window");

        cx.activate(true);
    });
}

/// Initialise `tracing` at the app edge.
///
/// Logs go to stderr; the level is `RUST_LOG` or `info` by default. Credentials
/// never reach a log line — the backend redacts passwords in `Debug` and maps
/// errors to credential-free messages (see `nyx-service`).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
