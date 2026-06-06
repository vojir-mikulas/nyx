// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! `nyx-ui` — an in-house GPUI component library and semantic theme layer.
//!
//! Built shadcn-style: we own the source, build on GPUI's `div`/styling
//! primitives, and expose a small set of themed components with a variant API.
//!
//! # Extraction rule
//!
//! This crate **must never depend on any `nyx-*` crate** or domain type. It is a
//! generic component library that happens to live in the Nyx repo today;
//! keeping it decoupled makes the future extraction into the standalone
//! **Flint** crate a near-trivial rename. See `docs/plans/plan-02-nyx-ui-flint.md`.
//!
//! # Usage
//!
//! ```ignore
//! use nyx_ui::prelude::*;
//! // install a theme once:
//! cx.set_global(Theme::one_dark());
//! // then read tokens in render:
//! div().bg(cx.theme().bg_app)
//! ```

pub mod components;
pub mod styled_ext;
pub mod theme;

mod tokens;

pub use components::button::{Button, ButtonSize, ButtonVariant};
pub use styled_ext::StyledExt;
pub use theme::{ActiveTheme, Theme};

/// Everything you need with a single `use nyx_ui::prelude::*;`.
pub mod prelude {
    pub use crate::components::button::{Button, ButtonSize, ButtonVariant};
    pub use crate::styled_ext::StyledExt;
    pub use crate::theme::{ActiveTheme, Theme};
}
