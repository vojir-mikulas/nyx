// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! `ContextMenu` — a floating list of actions (right-click / overflow menus).
//!
//! This component renders only the **menu surface** (the panel + items). Where
//! it appears is the caller's concern: wrap it in [`gpui::anchored`] /
//! [`gpui::deferred`] to pin it to a cursor position or a trigger button.
//!
//! ```ignore
//! ContextMenu::new("row-menu")
//!     .item(ContextMenuItem::new("download", "Download").shortcut("⌘D"))
//!     .separator()
//!     .item(ContextMenuItem::new("delete", "Delete").danger())
//! ```

use gpui::{div, prelude::*, App, ClickEvent, SharedString, Window};

use crate::theme::ActiveTheme;

/// A boxed click handler in GPUI's `(event, window, app)` shape.
type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A single actionable row in a [`ContextMenu`].
pub struct ContextMenuItem {
    key: SharedString,
    label: SharedString,
    shortcut: Option<SharedString>,
    danger: bool,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl ContextMenuItem {
    /// Create an item with a stable `key` (used as its element id) and a label.
    pub fn new(key: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            shortcut: None,
            danger: false,
            disabled: false,
            on_click: None,
        }
    }

    /// Show a keyboard-shortcut hint, right-aligned.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Render this item in the destructive (red) style.
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Disable the item (dimmed, no hover, no click).
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

/// One entry in the menu: an item or a visual separator.
enum Entry {
    Item(ContextMenuItem),
    Separator,
}

/// A floating list of actions.
#[derive(IntoElement)]
pub struct ContextMenu {
    id: SharedString,
    entries: Vec<Entry>,
}

impl ContextMenu {
    /// Create an empty menu with a stable `id`.
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            entries: Vec::new(),
        }
    }

    /// Append an action item.
    pub fn item(mut self, item: ContextMenuItem) -> Self {
        self.entries.push(Entry::Item(item));
        self
    }

    /// Append a separator line.
    pub fn separator(mut self) -> Self {
        self.entries.push(Entry::Separator);
        self
    }
}

impl RenderOnce for ContextMenu {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let menu_id = self.id;

        let rows = self.entries.into_iter().map(|entry| match entry {
            Entry::Separator => div()
                .h(gpui::px(1.))
                .mx(gpui::px(2.))
                .my(gpui::px(4.))
                .bg(theme.border_soft)
                .into_any_element(),
            Entry::Item(item) => {
                let base_color = if item.danger { theme.red } else { theme.text };
                let hover_bg = if item.danger { theme.red } else { theme.accent };
                let row = div()
                    .id(item.key.clone())
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .px_2p5()
                    .py_1p5()
                    .rounded(gpui::px(4.))
                    .text_color(base_color)
                    .child(div().flex_1().text_sm().child(item.label))
                    .when_some(item.shortcut, |this, sc| {
                        this.child(div().text_xs().text_color(theme.text_faint).child(sc))
                    });

                if item.disabled {
                    row.opacity(0.4).into_any_element()
                } else {
                    row.cursor_pointer()
                        .hover(move |s| s.bg(hover_bg).text_color(gpui::white()))
                        .when_some(item.on_click, |this, handler| {
                            this.on_click(move |event, window, cx| handler(event, window, cx))
                        })
                        .into_any_element()
                }
            }
        });

        div()
            .id(menu_id)
            .flex()
            .flex_col()
            .min_w(gpui::px(180.))
            .p_1()
            .bg(theme.bg_elevated)
            .border_1()
            .border_color(theme.border_strong)
            .rounded(gpui::px(7.))
            .shadow_lg()
            .children(rows)
    }
}
