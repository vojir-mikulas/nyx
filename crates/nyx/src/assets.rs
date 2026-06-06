// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! Embedded application assets — the vendored fonts and icon set — and the
//! [`gpui::AssetSource`] that serves them.
//!
//! Everything under the workspace `assets/` directory is baked into the binary
//! at compile time via [`rust_embed`], so the shipped app needs no sidecar
//! files. The icon SVGs are reached by `nyx`'s [`crate::icon`] helper through
//! `gpui::svg().path("icons/<name>.svg")`, which routes back here.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{App, AssetSource, SharedString};
use rust_embed::RustEmbed;

/// The vendored UI (sans) font family name, as registered with the text system.
pub const FONT_UI: &str = "IBM Plex Sans";
/// The vendored monospace font family name (paths, sizes, dates).
pub const FONT_MONO: &str = "JetBrains Mono";

/// The font files loaded into the text system at startup.
const FONT_FILES: &[&str] = &[
    "fonts/IBMPlexSans-Regular.ttf",
    "fonts/IBMPlexSans-Medium.ttf",
    "fonts/IBMPlexSans-SemiBold.ttf",
    "fonts/IBMPlexSans-Bold.ttf",
    "fonts/JetBrainsMono-Regular.ttf",
    "fonts/JetBrainsMono-Medium.ttf",
    "fonts/JetBrainsMono-Bold.ttf",
];

/// All embedded assets (fonts + icons), rooted at the workspace `assets/` dir.
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../assets"]
#[include = "fonts/*"]
#[include = "icons/*"]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Self::get(path).map(|file| file.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Self::iter()
            .filter(|p| p.starts_with(path))
            .map(|p| p.as_ref().into())
            .collect())
    }
}

impl Assets {
    /// Register the vendored fonts with the text system. Call once at startup.
    pub fn load_fonts(cx: &App) -> Result<()> {
        let fonts = FONT_FILES
            .iter()
            .filter_map(|path| Self::get(path).map(|file| file.data))
            .collect();
        cx.text_system().add_fonts(fonts)
    }
}
