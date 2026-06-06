// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The file browser: tab strip, breadcrumb toolbar, and the remote file table.

use std::rc::Rc;

use gpui::{div, prelude::*, px, Context, Hsla, SharedString};
use nyx_ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, Column, IconButton, Table, ToastVariant,
};

use crate::icon::icon;
use crate::state::AppState;

/// Render the browser column (tab strip + toolbar + table).
pub fn render(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(tab_strip(state, cx))
        .child(toolbar(state, cx))
        .child(file_table(state, cx))
}

fn tab_strip(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let (name, color) = state
        .active_conn()
        .map(|c| (c.profile.name.clone(), c.color.color(&theme)))
        .unwrap_or_else(|| ("—".into(), theme.text_faint));
    let dock_open = state.dock_open;

    div()
        .flex()
        .items_stretch()
        .h(px(36.))
        .flex_shrink_0()
        .bg(theme.bg_bar)
        .border_b_1()
        .border_color(theme.border_soft)
        .child(
            // Active connection tab.
            div()
                .relative()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .max_w(px(230.))
                .bg(theme.bg_app)
                .text_color(theme.text)
                .text_sm()
                .border_r_1()
                .border_color(theme.border_soft)
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top_0()
                        .h(px(1.))
                        .bg(theme.accent),
                )
                .child(div().text_color(color).child(icon("server", 13.)))
                .child(div().truncate().child(name))
                .child(
                    IconButton::new("tab-close", icon("x", 12.))
                        .size(nyx_ui::IconButtonSize::Xs)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.disconnect();
                            cx.notify();
                        })),
                ),
        )
        .child(div().flex_1())
        .child(
            div()
                .flex()
                .items_center()
                .gap_0p5()
                .px_2()
                .child(
                    IconButton::new("toggle-sidebar", icon("sidebarIc", 15.)).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.sidebar_open = !this.sidebar_open;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    IconButton::new("toggle-dock", icon("panelBottom", 15.))
                        .active(dock_open)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.dock_open = !this.dock_open;
                            cx.notify();
                        })),
                ),
        )
}

fn toolbar(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let can_back = state.can_back();
    let can_fwd = state.can_forward();
    let can_up = !state.cwd.is_empty();

    div()
        .flex()
        .items_center()
        .gap_1p5()
        .h(px(38.))
        .pl(px(10.))
        .pr_2()
        .flex_shrink_0()
        .bg(theme.bg_app)
        .border_b_1()
        .border_color(theme.border_soft)
        .child(
            div()
                .flex()
                .items_center()
                .child(
                    IconButton::new("nav-back", icon("arrowLeft", 16.))
                        .disabled(!can_back)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.back(cx);
                            cx.notify();
                        })),
                )
                .child(
                    IconButton::new("nav-fwd", icon("arrowRight", 16.))
                        .disabled(!can_fwd)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.forward(cx);
                            cx.notify();
                        })),
                )
                .child(
                    IconButton::new("nav-up", icon("arrowUp", 15.))
                        .disabled(!can_up)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.go_up(cx);
                            cx.notify();
                        })),
                )
                .child(
                    IconButton::new("nav-refresh", icon("refresh", 15.)).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.refresh(cx);
                            cx.notify();
                        },
                    )),
                ),
        )
        .child(separator(cx))
        .child(breadcrumb(state, cx))
        .child(div().w(px(200.)).child(state.filter.clone()))
        .child(separator(cx))
        .child(
            Button::new("new-folder", "New folder")
                .variant(ButtonVariant::Secondary)
                .size(ButtonSize::Sm)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.push_toast("New folder — coming in M4", ToastVariant::Info, cx);
                    cx.notify();
                })),
        )
        .child(
            Button::new("upload", "Upload")
                .variant(ButtonVariant::Primary)
                .size(ButtonSize::Sm)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.push_toast("Upload — coming in M5", ToastVariant::Info, cx);
                    cx.notify();
                })),
        )
}

fn separator(cx: &Context<AppState>) -> impl IntoElement {
    div()
        .w(px(1.))
        .h(px(18.))
        .mx_1()
        .bg(cx.theme().border)
        .flex_shrink_0()
}

fn breadcrumb(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let mut row = div()
        .id("crumbs")
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap_0p5()
        .overflow_hidden()
        .font_family(crate::assets::FONT_MONO)
        .text_sm();

    // Root crumb.
    row = row.child(crumb("/", 0, false, state.cwd.is_empty(), cx));
    for (i, seg) in state.cwd.iter().enumerate() {
        let is_last = i + 1 == state.cwd.len();
        row = row
            .child(div().text_color(theme.text_dim).child(icon("chevR", 12.)))
            .child(crumb(seg.clone(), i + 1, is_last, is_last, cx));
    }
    row
}

fn crumb(
    label: impl Into<SharedString>,
    index: usize,
    is_last: bool,
    highlighted: bool,
    cx: &mut Context<AppState>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let color = if highlighted {
        theme.text
    } else {
        theme.text_muted
    };
    div()
        .id(("crumb", index))
        .px_1p5()
        .py_0p5()
        .rounded(theme.radius_sm)
        .text_color(color)
        .whitespace_nowrap()
        .when(!is_last, |this| {
            this.cursor_pointer()
                .hover(|s| s.bg(theme.bg_hover).text_color(theme.text))
        })
        .child(label.into())
        .on_click(cx.listener(move |this, _, _, cx| {
            this.nav_crumb(index, cx);
            cx.notify();
        }))
}

/// One precomputed, owned table row (the `render_row` closure must be `'static`,
/// so it cannot borrow the listing).
struct VisibleRow {
    name: SharedString,
    is_dir: bool,
    icon_name: &'static str,
    icon_color: Hsla,
    name_color: Hsla,
    size: SharedString,
    modified: SharedString,
    type_label: SharedString,
    perms: SharedString,
}

fn file_table(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let show_perms = state.show_perms;
    let row_height = state.density.row_height();
    let mono = crate::assets::FONT_MONO;

    // Precompute the visible (filtered + sorted) rows as owned data.
    let visible = state.visible_entries(cx);
    let rows: Vec<VisibleRow> = visible
        .iter()
        .map(|row| {
            let (icon_name, icon_color) = row.icon(&theme);
            VisibleRow {
                name: row.entry.name.clone().into(),
                is_dir: row.entry.is_dir,
                icon_name,
                icon_color,
                name_color: if row.entry.is_dir {
                    theme.blue
                } else {
                    theme.text
                },
                size: row.display_size(),
                modified: row.display_modified(),
                type_label: row.type_label.clone(),
                perms: row.entry.perms.clone().into(),
            }
        })
        .collect();
    let rows = Rc::new(rows);

    // Indices of selected rows within the visible ordering.
    let selected_set = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| state.selected.contains(&r.name))
        .map(|(ix, _)| ix)
        .collect();

    let is_empty = rows.is_empty();
    let filter_active = !state.filter_text(cx).trim().is_empty();

    // Columns (perms only when toggled on).
    let mut columns = vec![
        Column::new("Name").flex().sortable(),
        Column::new("Size").width(px(96.)).align_end().sortable(),
        Column::new("Modified").width(px(150.)).sortable(),
        Column::new("Type").width(px(120.)).sortable(),
    ];
    if show_perms {
        columns.push(Column::new("Permissions").width(px(116.)));
    }

    let muted = theme.text_muted;
    let faint = theme.text_faint;
    let rows_for_render = rows.clone();
    let rows_for_select = rows.clone();
    let rows_for_activate = rows.clone();
    let view = cx.entity();

    let body: gpui::AnyElement = if is_empty {
        empty_state(filter_active, &state.filter_text(cx), cx).into_any_element()
    } else {
        Table::new("files", columns)
            .row_count(rows.len())
            .row_height(px(row_height))
            .selected_set(selected_set)
            .sort(Some((state.sort.0.column(), state.sort.1)))
            .on_sort({
                let view = view.clone();
                move |col, _window, cx| {
                    view.update(cx, |this, cx| {
                        this.toggle_sort(col);
                        cx.notify();
                    });
                }
            })
            .on_select({
                let view = view.clone();
                let rows = rows_for_select;
                move |ix, event, _window, cx| {
                    if let Some(row) = rows.get(ix) {
                        let mods = event.modifiers();
                        let additive = mods.platform || mods.control;
                        let name = row.name.clone();
                        view.update(cx, |this, cx| {
                            this.select(name, additive);
                            cx.notify();
                        });
                    }
                }
            })
            .on_activate({
                let view = view.clone();
                let rows = rows_for_activate;
                move |ix, _window, cx| {
                    if let Some(row) = rows.get(ix) {
                        if row.is_dir {
                            let name = row.name.clone();
                            view.update(cx, |this, cx| {
                                this.open_dir(&name, cx);
                                cx.notify();
                            });
                        }
                    }
                }
            })
            .render_row(move |ix, _window, _cx| {
                let row = &rows_for_render[ix];
                let mut cells = vec![
                    // Name: colored icon + name.
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .text_color(row.icon_color)
                                .child(icon(row.icon_name, 15.)),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_color(row.name_color)
                                .child(row.name.clone()),
                        )
                        .into_any_element(),
                    div()
                        .font_family(mono)
                        .text_color(muted)
                        .child(row.size.clone())
                        .into_any_element(),
                    div()
                        .font_family(mono)
                        .text_color(faint)
                        .child(row.modified.clone())
                        .into_any_element(),
                    div()
                        .text_color(faint)
                        .child(row.type_label.clone())
                        .into_any_element(),
                ];
                if show_perms {
                    cells.push(
                        div()
                            .font_family(mono)
                            .text_color(faint)
                            .child(row.perms.clone())
                            .into_any_element(),
                    );
                }
                cells
            })
            .into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .bg(theme.bg_app)
        .text_sm()
        .child(body)
}

fn empty_state(filter_active: bool, filter: &str, cx: &Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let label = if filter_active {
        format!("No matches for “{}”", filter.trim())
    } else {
        "This folder is empty".to_string()
    };
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1p5()
        .text_color(theme.text_dim)
        .child(div().opacity(0.5).child(icon("folderOpen", 26.)))
        .child(
            div()
                .font_family(crate::assets::FONT_MONO)
                .text_xs()
                .child(label),
        )
}
