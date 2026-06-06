// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! `Tabs` — a horizontal strip of selectable tabs with an optional count pill.
//!
//! Stateless: the caller owns the selected index and is told about clicks via
//! [`on_select`](Tabs::on_select). Built for the transfer dock (Active / Done /
//! Failed), but generic — tabs carry plain labels and counts, no domain types.
//!
//! ```ignore
//! Tabs::new("dock")
//!     .tab("Active", Some(3))
//!     .tab("Completed", Some(12))
//!     .selected(self.tab)
//!     .on_select(cx.listener(|this, ix: &usize, _, cx| { this.tab = *ix; cx.notify(); }))
//! ```

use gpui::{div, prelude::*, App, SharedString, Window};

use crate::theme::ActiveTheme;

/// A handler invoked with the clicked tab index.
type SelectHandler = Box<dyn Fn(usize, &mut Window, &mut App) + 'static>;

struct TabItem {
    label: SharedString,
    count: Option<usize>,
}

/// A horizontal strip of selectable tabs.
#[derive(IntoElement)]
pub struct Tabs {
    id: SharedString,
    items: Vec<TabItem>,
    selected: usize,
    on_select: Option<SelectHandler>,
}

impl Tabs {
    /// Create an empty tab strip with a stable `id`.
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            items: Vec::new(),
            selected: 0,
            on_select: None,
        }
    }

    /// Append a tab with a `label` and an optional `count` pill.
    pub fn tab(mut self, label: impl Into<SharedString>, count: Option<usize>) -> Self {
        self.items.push(TabItem {
            label: label.into(),
            count,
        });
        self
    }

    /// Set the selected tab index.
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Handler invoked with the index of a clicked tab.
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Tabs {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let selected = self.selected;
        let on_select = self.on_select.map(std::rc::Rc::new);

        let tabs = self.items.into_iter().enumerate().map(|(ix, item)| {
            let is_active = ix == selected;
            let (fg, bg) = if is_active {
                (theme.text, theme.bg_active)
            } else {
                (theme.text_faint, gpui::transparent_black())
            };
            let handler = on_select.clone();

            let pill = item.count.map(|count| {
                let (pill_fg, pill_bg) = if is_active {
                    (theme.on_accent, theme.accent)
                } else {
                    (theme.text_muted, theme.bg_elevated)
                };
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .min_w(gpui::px(16.))
                    .px_1()
                    .rounded(gpui::px(8.))
                    .text_xs()
                    .text_color(pill_fg)
                    .bg(pill_bg)
                    .child(count.to_string())
            });

            div()
                .id(ix)
                .flex()
                .items_center()
                .gap_1p5()
                .h(gpui::px(24.))
                .px_2p5()
                .rounded(theme.radius_sm)
                .text_sm()
                .text_color(fg)
                .bg(bg)
                .cursor_pointer()
                .when(!is_active, |this| {
                    this.hover(|s| s.bg(theme.bg_hover).text_color(theme.text_muted))
                })
                .child(item.label)
                .when_some(pill, |this, pill| this.child(pill))
                .when_some(handler, |this, handler| {
                    this.on_click(move |_, window, cx| handler(ix, window, cx))
                })
        });

        div()
            .id(self.id)
            .flex()
            .items_center()
            .gap_0p5()
            .children(tabs)
    }
}
