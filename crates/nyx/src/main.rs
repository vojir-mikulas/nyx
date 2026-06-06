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
        // Keyboard navigation is the pragmatic accessibility subset we ship for
        // the MVP: Tab traversal + Enter/Esc on inputs (`TextInput::bind_keys`)
        // and Enter/Backspace/F2/Delete in the browser (`browser::bind_keys`),
        // plus the theme's visible focus rings and contrast. A full platform a11y
        // tree (VoiceOver roles/labels) is deferred — the pinned GPUI exposes no
        // role/label surface to target, so it would have nowhere to land (D13).
        TextInput::bind_keys(cx);
        crate::views::browser::bind_keys(cx);
        crate::app::bind_keys(cx);

        let bounds = Bounds::centered(None, gpui::size(gpui::px(1100.0), gpui::px(720.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // Floor the window size so the chrome (sidebar + browser + dock)
                // always has room to lay out — no 1×1 windows.
                window_min_size: Some(gpui::size(gpui::px(720.), gpui::px(480.))),
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

        // Single-window app: closing the last window quits the process. Without
        // this, GPUI keeps its macOS event loop (and the backend thread) running
        // after the window is gone, so the app would linger with no UI.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.activate(true);
    });
}

/// Keeps the non-blocking log writer's worker thread alive for the whole
/// process. Dropping the guard flushes and stops the writer, so it must outlive
/// every log call — we stash it here at startup and never take it back out.
static LOG_GUARD: std::sync::OnceLock<tracing_appender::non_blocking::WorkerGuard> =
    std::sync::OnceLock::new();

/// Initialise `tracing` at the app edge.
///
/// Logs go to stderr **and** a daily-rolling file in the per-OS data dir; the
/// level is `RUST_LOG` or `info` by default. Credentials never reach a log line
/// — passwords live in the keychain only, the backend redacts them in `Debug`
/// and maps errors to credential-free messages (see `nyx-service`), and
/// `TransferProgress` stays below `info` so the file stays useful (plan M6 D14).
fn init_tracing() {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // A best-effort file layer: if the data dir can't be resolved, fall back to
    // stderr-only rather than failing to start.
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

/// The per-OS data directory for Nyx's log file (matching the profile store's
/// `dev/nyx/Nyx` identity), created if missing.
fn log_dir() -> Option<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("dev", "nyx", "Nyx")?;
    let dir = dirs.data_dir().to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}
