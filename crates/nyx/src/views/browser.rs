//! The file browser: tab strip, breadcrumb toolbar, and the remote file table.

use std::rc::Rc;

use gpui::{
    actions, div, prelude::*, px, radians, App, Context, ExternalPaths, Hsla, KeyBinding,
    MouseButton, SharedString, Transformation,
};
use nyx_ui::{ActiveTheme, Button, ButtonSize, ButtonVariant, Column, IconButton, Table};

use crate::icon::icon;
use crate::state::AppState;
use crate::views::titlebar_drag;

actions!(
    nyx_browser,
    [
        /// Open the selected entry (directory → list into it; file → download).
        Open,
        /// Go up one directory level.
        GoUp,
        /// Rename the single selected entry.
        Rename,
        /// Delete the current selection (confirmed).
        Delete,
    ]
);

/// Register the browser's keyboard bindings, scoped to the `"Browser"` key
/// context so they only fire when the file table (not an input) has focus.
/// Call once at startup.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some("Browser");
    cx.bind_keys([
        KeyBinding::new("enter", Open, ctx),
        KeyBinding::new("backspace", GoUp, ctx),
        KeyBinding::new("f2", Rename, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("cmd-backspace", Delete, ctx),
    ]);
}

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
    let sidebar_open = state.sidebar_open;

    titlebar_drag(
        div()
            .id("titlebar-right")
            .flex()
            .items_stretch()
            .h(px(36.))
            .flex_shrink_0()
            .bg(theme.bg_bar)
            .border_b_1()
            .border_color(theme.border_soft)
            // When the sidebar is hidden, left padding clears the macOS traffic lights.
            .when(!sidebar_open, |this| this.pl(px(80.)))
            .child(
                div()
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
                    .child(div().text_color(color).child(icon("server", 13., color)))
                    .child(div().truncate().child(name))
                    .child(
                        IconButton::new("tab-close", icon("x", 12., theme.text_faint))
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
                        IconButton::new("toggle-sidebar", icon("sidebarIc", 15., theme.text_faint))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_open = !this.sidebar_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        IconButton::new(
                            "toggle-dock",
                            icon(
                                "panelBottom",
                                15.,
                                if dock_open {
                                    theme.text
                                } else {
                                    theme.text_faint
                                },
                            ),
                        )
                        .active(dock_open)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.dock_open = !this.dock_open;
                            cx.notify();
                        })),
                    ),
            ),
    )
}

fn toolbar(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let can_back = state.can_back();
    let can_fwd = state.can_forward();
    let can_up = !state.cwd.is_root();

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
                    IconButton::new("nav-back", icon("arrowLeft", 16., theme.text_muted))
                        .disabled(!can_back)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.back(cx);
                            cx.notify();
                        })),
                )
                .child(
                    IconButton::new("nav-fwd", icon("arrowRight", 16., theme.text_muted))
                        .disabled(!can_fwd)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.forward(cx);
                            cx.notify();
                        })),
                )
                .child(
                    IconButton::new("nav-up", icon("arrowUp", 15., theme.text_muted))
                        .disabled(!can_up)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.go_up(cx);
                            cx.notify();
                        })),
                )
                .child(
                    IconButton::new("nav-refresh", icon("refresh", 15., theme.text_muted))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.refresh(cx);
                            cx.notify();
                        })),
                ),
        )
        .child(separator(cx))
        .child(breadcrumb(state, cx))
        .child(div().w(px(200.)).child(state.filter.clone()))
        .child(separator(cx))
        .child(
            IconButton::new("new-folder", icon("folderPlus", 15., theme.text_muted)).on_click(
                cx.listener(|this, _, _, cx| {
                    this.start_new_folder(cx);
                    cx.notify();
                }),
            ),
        )
        .child(
            Button::new("upload", "Upload")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Sm)
                .icon(icon("upload", 14., theme.text_muted))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.upload(cx);
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
        .overflow_x_scroll()
        .font_family(crate::assets::FONT_MONO)
        .text_sm();

    // Root crumb.
    let comps: Vec<&str> = state.cwd.components().collect();
    row = row.child(crumb("/", 0, false, comps.is_empty(), cx));
    for (i, seg) in comps.iter().enumerate() {
        let is_last = i + 1 == comps.len();
        row = row
            .child(div().flex_shrink_0().text_color(theme.text_dim).child(icon(
                "chevR",
                12.,
                theme.text_dim,
            )))
            .child(crumb(seg.to_string(), i + 1, is_last, is_last, cx));
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
        .flex_shrink_0()
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
    let caret_color = theme.text_muted;
    // Drop-zone tint shown while external files are dragged over the browser.
    let drop_zone = theme.accent_ghost;
    let rows_for_render = rows.clone();
    let rows_for_select = rows.clone();
    let rows_for_secondary = rows.clone();
    let rows_for_activate = rows.clone();
    let rows_for_drop = rows.clone();
    // Directory rows accept an external file drop (upload into that folder).
    let dir_rows: std::collections::HashSet<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.is_dir)
        .map(|(ix, _)| ix)
        .collect();
    let view = cx.entity();

    let body: gpui::AnyElement = if is_empty {
        empty_state(
            state.listing_loading,
            filter_active,
            &state.filter_text(cx),
            cx,
        )
        .into_any_element()
    } else {
        Table::new("files", columns)
            .row_count(rows.len())
            .row_height(px(row_height))
            .selected_set(selected_set)
            .sort(Some((state.sort.0.column(), state.sort.1)))
            .sort_carets(
                move || {
                    icon("chevD", 11., caret_color)
                        .with_transformation(Transformation::rotate(radians(std::f32::consts::PI)))
                        .into_any_element()
                },
                move || icon("chevD", 11., caret_color).into_any_element(),
            )
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
            .on_secondary({
                let view = view.clone();
                let rows = rows_for_secondary;
                move |ix, pos, _window, cx| {
                    if let Some(row) = rows.get(ix) {
                        let name = row.name.clone();
                        let is_dir = row.is_dir;
                        view.update(cx, |this, cx| {
                            this.open_file_menu(name, is_dir, pos);
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
            .droppable_rows(dir_rows)
            .on_row_drop({
                let view = view.clone();
                let rows = rows_for_drop;
                move |ix, paths, _window, cx| {
                    if let Some(row) = rows.get(ix) {
                        if row.is_dir {
                            let name = row.name.clone();
                            let files = paths.paths().to_vec();
                            view.update(cx, |this, cx| {
                                this.upload_paths(files, Some(name), cx);
                                cx.notify();
                            });
                        }
                    }
                }
            })
            .render_row(move |ix, _window, _cx| {
                let row = &rows_for_render[ix];
                let mut cells = vec![
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(div().text_color(row.icon_color).child(icon(
                            row.icon_name,
                            15.,
                            row.icon_color,
                        )))
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
        // The `"Browser"` key context wraps only the table, not the filter box,
        // so its keys never fight `TextInput` while the filter is focused. A
        // click in the table focuses it so the keys dispatch.
        .key_context("Browser")
        .track_focus(&state.browser_focus)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                window.focus(&this.browser_focus, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &Open, _, cx| {
            this.activate_selection(cx);
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &GoUp, _, cx| {
            this.go_up(cx);
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &Rename, _, cx| {
            this.rename_selection(cx);
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &Delete, _, cx| {
            this.start_delete(cx);
            cx.notify();
        }))
        // Drag external files into the browser → upload to the current directory
        // (a folder row instead targets that folder, via the row-drop above).
        .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_zone))
        .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
            this.upload_paths(paths.paths().to_vec(), None, cx);
            cx.notify();
        }))
        .child(body)
}

fn empty_state(
    loading: bool,
    filter_active: bool,
    filter: &str,
    cx: &Context<AppState>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let label = if loading {
        "Loading…".to_string()
    } else if filter_active {
        format!("No matches for “{}”", filter.trim())
    } else {
        "This folder is empty".to_string()
    };
    // While loading, show the minimal spinner; otherwise a faint folder glyph.
    let indicator: gpui::AnyElement = if loading {
        crate::icon::spinner("browser-loading", 24., theme.text_dim).into_any_element()
    } else {
        div()
            .opacity(0.5)
            .child(icon("folderOpen", 26., theme.text_dim))
            .into_any_element()
    };
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1p5()
        .text_color(theme.text_dim)
        .child(indicator)
        .child(
            div()
                .font_family(crate::assets::FONT_MONO)
                .text_xs()
                .child(label),
        )
}
