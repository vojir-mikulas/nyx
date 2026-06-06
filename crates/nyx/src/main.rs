//! The Nyx application binary.
//!
//! Assembles the app shell — sidebar, welcome / connection manager, file
//! browser, transfer dock and status bar. The single root [`AppState`] entity
//! owns all state and interaction logic.

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
        crate::views::browser::bind_keys(cx);
        crate::app::bind_keys(cx);

        let bounds = Bounds::centered(None, gpui::size(gpui::px(1100.0), gpui::px(720.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(gpui::size(gpui::px(720.), gpui::px(480.))),
                // Seamless titlebar: hide the native bar, inset the macOS traffic
                // lights into the top-left strip.
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

        // Closing the last window must quit, or GPUI's event loop lingers with no UI.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.activate(true);
    });
}

/// Must outlive every log call: dropping the guard flushes and stops the
/// non-blocking writer. Stashed here at startup, never taken back out.
static LOG_GUARD: std::sync::OnceLock<tracing_appender::non_blocking::WorkerGuard> =
    std::sync::OnceLock::new();

/// Initialise `tracing`: stderr plus a daily-rolling file in the per-OS data
/// dir. Level is `RUST_LOG` or `info`. Credentials never reach a log line.
fn init_tracing() {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // Best-effort: if the data dir can't be resolved, fall back to stderr-only.
    let file_layer = log_dir().map(|dir| {
        let appender = tracing_appender::rolling::daily(dir, "nyx.log");
        let (writer, guard) = tracing_appender::non_blocking(appender);
        let _ = LOG_GUARD.set(guard);
        fmt::layer().with_ansi(false).with_writer(writer)
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(file_layer)
        .init();
}

/// The per-OS data directory for Nyx's log file, created if missing.
fn log_dir() -> Option<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("dev", "nyx", "Nyx")?;
    let dir = dirs.data_dir().to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}
