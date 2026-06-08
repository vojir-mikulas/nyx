//! The file browser: tab strip, breadcrumb toolbar, and the remote file table.

use gpui::{
    actions, canvas, div, prelude::*, px, quad, radians, BorderStyle, Context, DragMoveEvent,
    ExternalPaths, MouseButton, MouseDownEvent, MouseMoveEvent, SharedString, Transformation,
};
use nyx_core::Protocol;
use nyx_ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, Column, IconButton, Table, Theme, Tooltip,
};

use crate::icon::icon;
use crate::state::models::EntryRow;
use crate::state::{AppState, SearchState};
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
        /// Move the selection up one row.
        SelectUp,
        /// Move the selection down one row.
        SelectDown,
        /// Select the first row.
        SelectFirst,
        /// Select the last row.
        SelectLast,
        /// Select every visible row.
        SelectAllRows,
        /// Copy the selected entry's remote path to the clipboard.
        CopyPath,
    ]
);

/// In-app drag payload: the rows being dragged within the browser (the whole
/// selection when the grabbed row is part of it, otherwise just that row).
/// Dropped on a folder row it's a server-side move; dragged out of the window it
/// is promoted to a native OS drag-out. Kept in the app crate so `nyx-ui` stays
/// domain-agnostic (the table is generic over this payload).
#[derive(Clone)]
pub struct InAppDrag {
    pub names: Vec<SharedString>,
}

/// Render the browser column (tab strip + toolbar + table or search results).
pub fn render(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    // A `/`-scoped filter swaps the directory table for streamed tree-search
    // results; otherwise the normal file table.
    let body: gpui::AnyElement = if state.search().is_some() {
        search_view(state, cx).into_any_element()
    } else {
        file_table(state, cx).into_any_element()
    };
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(tab_strip(state, cx))
        .when_some(state.connection_lost.clone(), |this, reason| {
            this.child(connection_lost_banner(state, reason, cx))
        })
        .child(toolbar(state, cx))
        .child(body)
}

/// The non-modal connection banner: stays put under the tab strip, leaving the
/// last listing visible beneath it. It reflects three states -
/// auto-reconnecting (title "Reconnecting…", a **Cancel**), gave-up
/// ("Reconnect failed", a **Reconnect**), and a plain manual loss ("Connection
/// lost", a **Reconnect**).
fn connection_lost_banner(
    state: &AppState,
    reason: SharedString,
    cx: &mut Context<AppState>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let attempt = state.reconnect_attempt;
    let title = if attempt.is_some() {
        "Reconnecting…"
    } else if state.reconnect_failed {
        "Reconnect failed"
    } else {
        "Connection lost"
    };
    // While auto-reconnecting, the detail shows the attempt count; otherwise the
    // credential-free loss reason.
    let detail: SharedString = match attempt {
        Some(n) => format!("Attempt {n}").into(),
        None => reason,
    };
    let action = if attempt.is_some() {
        Button::new("cancel-reconnect", "Cancel")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Sm)
            .focusable(false)
            .on_click(cx.listener(|this, _, _, cx| {
                this.cancel_reconnect();
                cx.notify();
            }))
    } else {
        Button::new("reconnect", "Reconnect")
            .variant(ButtonVariant::Primary)
            .size(ButtonSize::Sm)
            .focusable(false)
            .on_click(cx.listener(|this, _, _, cx| {
                this.reconnect(cx);
                cx.notify();
            }))
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .h(px(34.))
        .flex_shrink_0()
        .px_3()
        .bg(theme.red.opacity(0.10))
        .border_b_1()
        .border_color(theme.border_soft)
        .text_sm()
        .child(
            div()
                .text_color(theme.red)
                .child(icon("alert", 14., theme.red)),
        )
        .child(div().text_color(theme.text).child(title))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(theme.text_faint)
                .child(detail),
        )
        .child(action)
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
                    .bg(color.opacity(0.10))
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
                        IconButton::new(
                            "toggle-sidebar",
                            icon(
                                "sidebarIc",
                                15.,
                                if sidebar_open {
                                    theme.text
                                } else {
                                    theme.text_faint
                                },
                            ),
                        )
                        .active(sidebar_open)
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
                .focusable(false)
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
/// Resolve a table row index (a position in the visible order) to its entry.
fn row_at<'a>(listing: &'a [EntryRow], order: &[usize], ix: usize) -> Option<&'a EntryRow> {
    order.get(ix).and_then(|&i| listing.get(i))
}

fn file_table(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let show_perms = state.show_perms;
    let row_height = state.density.row_height();
    let mono = crate::assets::FONT_MONO;

    // The listing and its visible order are precomputed and cached on the state;
    // here we only clone the `Rc`s and hand them to the row closures, so nothing
    // O(n) runs per frame - rows are formatted lazily for the visible range only.
    let listing = state.listing.clone();
    let order = state.view_order();
    let row_count = order.len();
    let is_empty = row_count == 0;
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
    // Reset the folder-row rect cache; the table's paint repopulates it. Used to
    // resolve an OS drag-out that returns inside the window (Phase 3 re-entry).
    state.clear_drop_row_bounds();
    let bounds_sink = state.drop_row_bounds_sink();
    // Snapshot of the current selection, to mint the drag payload and size the
    // drag-preview count badge (the drag closures must be `'static`).
    let selected_for_drag = state.selected.clone();
    let selected_for_preview = state.selected.clone();
    let chip_theme = theme.clone();
    // While a native drag-out is back inside the window, highlight the folder
    // under the cursor (the OS cursor can't revert, so this is the drop cue).
    let drag_return_folder = state.drag_return_folder.clone();
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
            .row_count(row_count)
            .row_height(px(row_height))
            .track_scroll(state.file_scroll())
            .selected_set({
                let listing = listing.clone();
                let order = order.clone();
                let selected = state.selected.clone();
                move |ix| {
                    row_at(&listing, &order, ix)
                        .is_some_and(|row| selected.contains(row.entry.name.as_str()))
                }
            })
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
                let listing = listing.clone();
                let order = order.clone();
                move |ix, event, _window, cx| {
                    if let Some(row) = row_at(&listing, &order, ix) {
                        let mods = event.modifiers();
                        let additive = mods.platform || mods.control;
                        let name = SharedString::from(row.entry.name.clone());
                        view.update(cx, |this, cx| {
                            // Shift-click extends a range from the anchor; cmd/ctrl
                            // toggles; a plain click replaces.
                            if mods.shift {
                                this.select_range(name);
                            } else {
                                this.select(name, additive);
                            }
                            cx.notify();
                        });
                    }
                }
            })
            .on_secondary({
                let view = view.clone();
                let listing = listing.clone();
                let order = order.clone();
                move |ix, pos, _window, cx| {
                    if let Some(row) = row_at(&listing, &order, ix) {
                        let name = SharedString::from(row.entry.name.clone());
                        view.update(cx, |this, cx| {
                            this.open_file_menu(name, pos);
                            cx.notify();
                        });
                    }
                }
            })
            .on_activate({
                let view = view.clone();
                let listing = listing.clone();
                let order = order.clone();
                move |ix, _window, cx| {
                    if let Some(row) = row_at(&listing, &order, ix) {
                        // A directory opens, a symlink resolves (navigate or
                        // download); a plain file does nothing on double-click.
                        let name = SharedString::from(row.entry.name.clone());
                        view.update(cx, |this, cx| {
                            this.activate_row(&name, cx);
                            cx.notify();
                        });
                    }
                }
            })
            // Every row starts an in-app drag (a move when dropped on a folder,
            // promoted to a native drag-out when the pointer leaves the window).
            .draggable_rows(|_| true)
            .highlighted_rows({
                let listing = listing.clone();
                let order = order.clone();
                move |ix| {
                    drag_return_folder.as_ref().is_some_and(|folder| {
                        row_at(&listing, &order, ix)
                            .is_some_and(|row| row.entry.name.as_str() == folder.as_ref())
                    })
                }
            })
            // Start an in-app drag: drag the whole selection if the grabbed row is
            // part of it, else just that row.
            .on_row_drag({
                let listing = listing.clone();
                let order = order.clone();
                move |ix| {
                    row_at(&listing, &order, ix).map(|row| {
                        let name = SharedString::from(row.entry.name.clone());
                        let names = if selected_for_drag.contains(name.as_str()) {
                            selected_for_drag.iter().cloned().collect()
                        } else {
                            vec![name]
                        };
                        InAppDrag { names }
                    })
                }
            })
            .drag_preview({
                let listing = listing.clone();
                let order = order.clone();
                move |ix, _window, _cx| {
                    let Some(row) = row_at(&listing, &order, ix) else {
                        return div().into_any_element();
                    };
                    let count = if selected_for_preview.contains(row.entry.name.as_str()) {
                        selected_for_preview.len()
                    } else {
                        1
                    };
                    drag_chip(row, count, &chip_theme)
                }
            })
            // Directory rows accept a dropped item - an external file (upload) or
            // an in-app selection (move) into that folder.
            .droppable_rows({
                let listing = listing.clone();
                let order = order.clone();
                move |ix| row_at(&listing, &order, ix).is_some_and(|row| row.entry.is_dir())
            })
            .on_row_drop({
                let view = view.clone();
                let listing = listing.clone();
                let order = order.clone();
                move |ix, paths, _window, cx| {
                    if let Some(row) = row_at(&listing, &order, ix) {
                        if row.entry.is_dir() {
                            let name = SharedString::from(row.entry.name.clone());
                            let files = paths.paths().to_vec();
                            view.update(cx, |this, cx| {
                                this.upload_paths(files, Some(name), cx);
                                cx.notify();
                            });
                        }
                    }
                }
            })
            // Drop the in-app selection onto a folder row → server-side move.
            .on_row_drop_item({
                let view = view.clone();
                let listing = listing.clone();
                let order = order.clone();
                move |ix, drag: &InAppDrag, _window, cx| {
                    if let Some(row) = row_at(&listing, &order, ix) {
                        if row.entry.is_dir() {
                            let dir = SharedString::from(row.entry.name.clone());
                            let names = drag.names.clone();
                            view.update(cx, |this, cx| {
                                this.move_into(&dir, names, cx);
                                cx.notify();
                            });
                        }
                    }
                }
            })
            // Record folder-row rects each paint so a returning OS drag-out can be
            // resolved to the folder under the drop point.
            .on_row_bounds({
                let listing = listing.clone();
                let order = order.clone();
                move |ix, bounds, _window, _cx| {
                    if let Some(row) = row_at(&listing, &order, ix) {
                        if row.entry.is_dir() {
                            bounds_sink
                                .borrow_mut()
                                .push((SharedString::from(row.entry.name.clone()), bounds));
                        }
                    }
                }
            })
            // Rows are formatted lazily here, for the visible range only.
            .render_row({
                let theme = theme.clone();
                move |ix, _window, _cx| {
                    let Some(row) = row_at(&listing, &order, ix) else {
                        return Vec::new();
                    };
                    let (icon_name, icon_color) = row.icon(&theme);
                    let name_color = if row.entry.is_dir() {
                        theme.blue
                    } else {
                        theme.text
                    };
                    let mut cells = vec![
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .text_color(icon_color)
                                    .child(icon(icon_name, 15., icon_color)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_color(name_color)
                                    .child(SharedString::from(row.entry.name.clone())),
                            )
                            .into_any_element(),
                        div()
                            .font_family(mono)
                            .text_color(muted)
                            .child(row.display_size())
                            .into_any_element(),
                        div()
                            .font_family(mono)
                            .text_color(faint)
                            .child(row.display_modified())
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
                                .child(row.display_perms())
                                .into_any_element(),
                        );
                    }
                    cells
                }
            })
            .into_any_element()
    };

    // The rubber-band rectangle (window coords), painted over the rows once the
    // drag passes the start threshold.
    let marquee = state.marquee_rect();
    let marquee_fill = {
        let mut c = theme.accent;
        c.a = 0.12;
        c
    };
    let marquee_border = theme.accent;

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .relative()
        .bg(theme.bg_app)
        .text_sm()
        .track_focus(&state.browser_focus)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                window.focus(&this.browser_focus, cx);
                // A press on empty space (never over a row) begins a rubber-band.
                this.marquee_start(ev.position, cx);
            }),
        )
        // Grow the rubber-band while the left button is held; ends on release.
        .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
            if ev.pressed_button == Some(MouseButton::Left) {
                this.marquee_update(ev.position, cx);
            } else {
                // The button was released (possibly off the table, so on_mouse_up
                // never fired) - tidy up any rubber-band left dangling.
                this.marquee_end(cx);
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _window, cx| {
                this.marquee_end(cx);
            }),
        )
        // The `"Browser"` key context wraps only the table, not the filter box, so
        // its keys never fight `TextInput` while the filter is focused. A click in
        // the table focuses it so the keys dispatch. While any overlay is open the
        // context is dropped so dialog keys (Enter/Esc) route to the modal instead
        // of leaking to the table beneath it.
        .when(!state.has_overlay(), |this| {
            this.key_context("Browser")
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
                .on_action(cx.listener(|this, _: &SelectUp, _, cx| {
                    this.move_selection(-1);
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &SelectDown, _, cx| {
                    this.move_selection(1);
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                    this.select_edge(false);
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                    this.select_edge(true);
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &SelectAllRows, _, cx| {
                    this.select_all_visible();
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &CopyPath, _, cx| {
                    this.copy_selection_path(cx);
                    cx.notify();
                }))
        })
        // Drag external files into the browser → upload to the current directory
        // (a folder row instead targets that folder, via the row-drop above).
        .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_zone))
        .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
            this.upload_paths(paths.paths().to_vec(), None, cx);
            cx.notify();
        }))
        // Auto-promote an in-app drag to a native OS drag-out the moment the
        // pointer crosses out of the window. A small margin keeps a drag hugging
        // the edge from flickering between the two mechanisms. Capture-phase, so
        // it fires even while the pointer is over child rows.
        .on_drag_move(
            cx.listener(|this, ev: &DragMoveEvent<InAppDrag>, window, cx| {
                let size = window.viewport_size();
                let pos = window.mouse_position();
                let margin = px(2.);
                let outside = pos.x < margin
                    || pos.y < margin
                    || pos.x > size.width - margin
                    || pos.y > size.height - margin;
                if outside {
                    let names = ev.drag(cx).names.clone();
                    this.handoff_drag_out(names, window, cx);
                }
            }),
        )
        .child(body)
        // The rubber-band overlay: a no-hitbox canvas so it never intercepts the
        // clicks/drags beneath it. Paints in window coordinates, matching the
        // recorded row rects the selection hit-tests.
        .when_some(marquee, |this, rect| {
            this.child(
                canvas(
                    |_, _, _| (),
                    move |_bounds, _, window, _cx| {
                        window.paint_quad(quad(
                            rect,
                            px(1.),
                            marquee_fill,
                            px(1.),
                            marquee_border,
                            BorderStyle::Solid,
                        ));
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
        })
}

/// The floating preview shown under the cursor during an in-app drag: a small
/// chip with the grabbed row's icon and name, plus a count badge when more than
/// one row is being dragged.
fn drag_chip(row: &EntryRow, count: usize, theme: &Theme) -> gpui::AnyElement {
    let (icon_name, icon_color) = row.icon(theme);
    let mut chip = div()
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .bg(theme.bg_elevated)
        .border_1()
        .border_color(theme.border)
        .text_sm()
        .text_color(theme.text)
        .child(
            div()
                .text_color(icon_color)
                .child(icon(icon_name, 14., icon_color)),
        )
        .child(
            div()
                .max_w(px(180.))
                .truncate()
                .child(SharedString::from(row.entry.name.clone())),
        );
    if count > 1 {
        chip = chip.child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .min_w(px(16.))
                .h(px(16.))
                .pl(px(5.))
                .pr(px(3.))
                .rounded_full()
                .bg(theme.accent)
                .text_color(theme.on_accent)
                .text_size(px(10.))
                .line_height(px(16.))
                .child(format!("{count}")),
        );
    }
    chip.into_any_element()
}

/// The tree-search results view: a status header over a table of hits, shown in
/// place of the file table while a `/`-scoped filter is active. Deliberately
/// simpler than the file table - no drag/drop or rename, just open-on-activate
/// (which navigates to the hit's folder).
fn search_view(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let search = state
        .search()
        .expect("search_view requires an active search");
    // FTP/FTPS have no `find`: the service crawls the tree client-side, which is
    // markedly slower on deep trees. SFTP runs server-side `find` where it can.
    let client_side_walk = matches!(
        state.active_conn().map(|c| c.profile.protocol),
        Some(Protocol::Ftp | Protocol::Ftps)
    );
    let rows = search.hits.clone();
    let row_count = rows.len();
    let row_height = state.density.row_height();
    let mono = crate::assets::FONT_MONO;
    let muted = theme.text_muted;
    let faint = theme.text_faint;
    let view = cx.entity();

    let columns = vec![
        Column::new("Name").flex(),
        Column::new("Path").flex(),
        Column::new("Size").width(px(96.)).align_end(),
        Column::new("Modified").width(px(150.)),
        Column::new("Type").width(px(120.)),
    ];

    let body: gpui::AnyElement = if row_count == 0 {
        search_empty_state(search, cx).into_any_element()
    } else {
        Table::<InAppDrag>::new("search-results", columns)
            .row_count(row_count)
            .row_height(px(row_height))
            .on_activate({
                let view = view.clone();
                let rows = rows.clone();
                move |ix, _window, cx| {
                    if let Some(hit) = rows.get(ix) {
                        let path = hit.path.clone();
                        view.update(cx, |this, cx| {
                            this.activate_search_hit(&path, cx);
                            cx.notify();
                        });
                    }
                }
            })
            .render_row({
                let theme = theme.clone();
                let rows = rows.clone();
                move |ix, _window, _cx| {
                    let Some(hit) = rows.get(ix) else {
                        return Vec::new();
                    };
                    let (icon_name, icon_color) = hit.row.icon(&theme);
                    let name_color = if hit.row.entry.is_dir() {
                        theme.blue
                    } else {
                        theme.text
                    };
                    vec![
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .text_color(icon_color)
                                    .child(icon(icon_name, 15., icon_color)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_color(name_color)
                                    .child(SharedString::from(hit.row.entry.name.clone())),
                            )
                            .into_any_element(),
                        div()
                            .truncate()
                            .font_family(mono)
                            .text_color(faint)
                            .child(hit.parent.clone())
                            .into_any_element(),
                        div()
                            .font_family(mono)
                            .text_color(muted)
                            .child(hit.row.display_size())
                            .into_any_element(),
                        div()
                            .font_family(mono)
                            .text_color(faint)
                            .child(hit.row.display_modified())
                            .into_any_element(),
                        div()
                            .text_color(faint)
                            .child(hit.row.type_label.clone())
                            .into_any_element(),
                    ]
                }
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
        .track_focus(&state.browser_focus)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                window.focus(&this.browser_focus, cx);
            }),
        )
        .child(search_header(search, client_side_walk, &theme))
        .child(div().flex_1().min_h_0().child(body))
}

/// The thin status bar above the search results: a spinner while the walk runs,
/// the query, and a match count (with a "capped" hint when truncated).
fn search_header(search: &SearchState, client_side_walk: bool, theme: &Theme) -> impl IntoElement {
    let count = search.hits.len();
    let status = if !search.done {
        format!("Searching… {count} found")
    } else if count == 0 {
        "No matches".to_string()
    } else if search.truncated {
        format!("{count} matches (capped - narrow your search)")
    } else if count == 1 {
        "1 match".to_string()
    } else {
        format!("{count} matches")
    };
    let indicator: gpui::AnyElement = if search.done {
        icon("search", 13., theme.text_muted).into_any_element()
    } else {
        crate::icon::spinner("search-spinner", 13., theme.text_muted).into_any_element()
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .h(px(30.))
        .flex_shrink_0()
        .px_3()
        .bg(theme.bg_app)
        .border_b_1()
        .border_color(theme.border_soft)
        .text_xs()
        .text_color(theme.text_muted)
        .child(indicator)
        .child(
            div()
                .max_w(px(280.))
                .truncate()
                .font_family(crate::assets::FONT_MONO)
                .child(search.query.clone()),
        )
        .when(client_side_walk, |row| {
            row.child(
                div()
                    .id("search-slow-warning")
                    .flex()
                    .items_center()
                    .child(icon("alert", 13., theme.yellow))
                    .tooltip(Tooltip::text(
                        "FTP has no server-side search, so Nyx crawls the tree from \
                         here - this can be slow on large directories.",
                    )),
            )
        })
        .child(div().flex_1())
        .child(div().text_color(theme.text_faint).child(status))
}

/// The empty body while a search is running (spinner) or finished with no hits.
fn search_empty_state(search: &SearchState, cx: &Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let (indicator, label): (gpui::AnyElement, String) = if search.done {
        (
            div()
                .opacity(0.5)
                .child(icon("search", 26., theme.text_dim))
                .into_any_element(),
            format!("No matches for “{}”", search.query.trim()),
        )
    } else {
        (
            crate::icon::spinner("search-empty", 24., theme.text_dim).into_any_element(),
            "Searching…".to_string(),
        )
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
