// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! `IconButton` — a compact, square, icon-only button (toolbars, row actions).
//!
//! It stays domain-free by taking its glyph as a generic `impl IntoElement`
//! child rather than any icon enum: the app supplies the icon element (an
//! `svg()`, a styled `div`, a character), and `IconButton` provides the themed
//! hit-target, hover, active and disabled states.
//!
//! ```ignore
//! IconButton::new("refresh", svg().path("icons/refresh.svg"))
//!     .size(IconButtonSize::Md)
//!     .on_click(|_, _, _| { /* … */ });
//! ```

use gpui::{div, prelude::*, AnyElement, App, ClickEvent, ElementId, Window};

use crate::theme::ActiveTheme;

/// A boxed click handler in GPUI's `(event, window, app)` shape.
type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Size of an [`IconButton`] (the square edge length, in pixels).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IconButtonSize {
    /// 22px — dense row actions.
    Xs,
    /// 24px — toolbars (the default).
    #[default]
    Sm,
    /// 28px — prominent affordances.
    Md,
}

impl IconButtonSize {
    fn edge(self) -> f32 {
        match self {
            IconButtonSize::Xs => 22.0,
            IconButtonSize::Sm => 24.0,
            IconButtonSize::Md => 28.0,
        }
    }
}

/// A themed, square, icon-only button.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: AnyElement,
    size: IconButtonSize,
    active: bool,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    /// Create a button with a stable `id` and an `icon` element.
    pub fn new(id: impl Into<ElementId>, icon: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            icon: icon.into_any_element(),
            size: IconButtonSize::default(),
            active: false,
            disabled: false,
            on_click: None,
        }
    }

    /// Set the size.
    pub fn size(mut self, size: IconButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Mark the button as active (toggled-on, persistent highlight).
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Mark the button disabled (no hover, no click, dimmed).
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Attach a click handler.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let edge = gpui::px(self.size.edge());
        let theme = cx.theme();

        let (fg, bg) = if self.active {
            (theme.text, theme.bg_active)
        } else {
            (theme.text_faint, gpui::transparent_black())
        };
        let hover_bg = theme.bg_hover;
        let hover_fg = theme.text_muted;

        let base = div()
            .id(self.id)
            .flex()
            .items_center()
            .justify_center()
            .size(edge)
            .rounded(theme.radius_sm)
            .text_color(fg)
            .bg(bg)
            .child(self.icon);

        let interactive = if self.disabled {
            base.opacity(0.4)
        } else {
            base.cursor_pointer()
                .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
        };

        match (self.disabled, self.on_click) {
            (false, Some(handler)) => {
                interactive.on_click(move |event, window, cx| handler(event, window, cx))
            }
            _ => interactive,
        }
    }
}
