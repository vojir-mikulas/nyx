// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! `ProgressBar` — a thin determinate / indeterminate progress track.
//!
//! Mirrors the design's `.xbar` (transfer dock): a recessed track with an
//! accent fill. Determinate fills to a `0.0..=1.0` fraction; indeterminate
//! animates a sweeping segment for unknown-duration work.
//!
//! ```ignore
//! ProgressBar::new("upload", 0.62);          // 62%
//! ProgressBar::new("scan", 0.0).indeterminate(true);
//! ```

use std::time::Duration;

use gpui::{
    div, ease_in_out, prelude::*, px, relative, Animation, AnimationExt, App, ElementId, Window,
};

use crate::theme::ActiveTheme;

/// A thin progress track with an accent fill.
#[derive(IntoElement)]
pub struct ProgressBar {
    id: ElementId,
    fraction: f32,
    indeterminate: bool,
}

impl ProgressBar {
    /// Create a determinate bar filled to `fraction` (clamped to `0.0..=1.0`).
    pub fn new(id: impl Into<ElementId>, fraction: f32) -> Self {
        Self {
            id: id.into(),
            fraction: fraction.clamp(0.0, 1.0),
            indeterminate: false,
        }
    }

    /// Switch to an indeterminate (animated sweep) bar.
    pub fn indeterminate(mut self, indeterminate: bool) -> Self {
        self.indeterminate = indeterminate;
        self
    }
}

impl RenderOnce for ProgressBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let accent = theme.accent;

        let track = div()
            .relative()
            .h(px(4.0))
            .w_full()
            .rounded(px(3.0))
            .bg(theme.bg_input)
            .overflow_hidden();

        if self.indeterminate {
            // A 40%-wide segment sweeps from off-screen-left to off-screen-right.
            track.child(
                div()
                    .absolute()
                    .top_0()
                    .h_full()
                    .w(relative(0.4))
                    .rounded(px(3.0))
                    .bg(accent)
                    .with_animation(
                        self.id,
                        Animation::new(Duration::from_millis(1100))
                            .repeat()
                            .with_easing(ease_in_out),
                        |segment, delta| segment.left(relative(-0.4 + 1.4 * delta)),
                    ),
            )
        } else {
            track.child(
                div()
                    .h_full()
                    .w(relative(self.fraction))
                    .rounded(px(3.0))
                    .bg(accent),
            )
        }
    }
}
