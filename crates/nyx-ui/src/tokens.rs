// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! Concrete token tables for the built-in themes, ported from
//! `design/styles.css`. Add new themes here as additional `Theme` constructors.

use gpui::{px, rgb, Hsla};

use crate::theme::Theme;

/// Hex (`0xRRGGBB`) → opaque [`Hsla`].
fn h(hex: u32) -> Hsla {
    rgb(hex).into()
}

impl Theme {
    /// Zed's **One Dark** — the primary theme.
    pub fn one_dark() -> Self {
        Theme {
            name: "One Dark",

            bg_app: h(0x282c33),
            bg_panel: h(0x21242b),
            bg_panel_2: h(0x1d2026),
            bg_elevated: h(0x2f343e),
            bg_bar: h(0x24272e),
            bg_hover: h(0x2c313a),
            bg_active: h(0x323843),
            bg_selected: h(0x2e3b54),
            bg_input: h(0x1b1e24),

            border: h(0x383d48),
            border_soft: h(0x2d313a),
            border_strong: h(0x464c59),

            text: h(0xdce0e5),
            text_muted: h(0x9aa1ad),
            text_faint: h(0x6b7280),
            text_dim: h(0x565d68),

            accent: h(0x5d80e6),
            accent_hover: h(0x6e8eea),
            accent_ghost: h(0x5d80e6).opacity(0.14),
            on_accent: h(0xf3f6ff),

            green: h(0x98c379),
            red: h(0xe06c75),
            blue: h(0x61afef),
            purple: h(0xc678dd),
            yellow: h(0xe5c07b),
            orange: h(0xd19a66),

            row_height: px(26.0),
            radius: px(5.0),
            radius_sm: px(3.0),
        }
    }

    /// **GitHub Dark** — the alternate theme.
    pub fn github_dark() -> Self {
        Theme {
            name: "GitHub Dark",

            bg_app: h(0x0d1117),
            bg_panel: h(0x010409),
            bg_panel_2: h(0x010409),
            bg_elevated: h(0x161b22),
            bg_bar: h(0x161b22),
            bg_hover: h(0x161b22),
            bg_active: h(0x21262d),
            bg_selected: h(0x15243d),
            bg_input: h(0x010409),

            border: h(0x30363d),
            border_soft: h(0x21262d),
            border_strong: h(0x444c56),

            text: h(0xe6edf3),
            text_muted: h(0x7d8590),
            text_faint: h(0x6e7681),
            text_dim: h(0x545d68),

            accent: h(0x2f81f7),
            accent_hover: h(0x4895ff),
            accent_ghost: h(0x2f81f7).opacity(0.15),
            on_accent: h(0xffffff),

            green: h(0x3fb950),
            red: h(0xf85149),
            blue: h(0x79c0ff),
            purple: h(0xd2a8ff),
            yellow: h(0xe3b341),
            orange: h(0xffa657),

            row_height: px(26.0),
            radius: px(5.0),
            radius_sm: px(3.0),
        }
    }
}
