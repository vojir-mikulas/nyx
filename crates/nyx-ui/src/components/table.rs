// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! `Table` — a virtualized, fixed-row-height data table (the file browser's
//! backbone), built on GPUI's [`uniform_list`](gpui::uniform_list).
//!
//! It is **fully generic**: it knows nothing about the data. The caller declares
//! [`Column`]s (width / alignment / sortability) and supplies a row renderer
//! closure that maps a row index to one cell element per column. Selection and
//! sort are *stateless* here — the table renders the state it is given and
//! reports clicks via [`on_select`](Table::on_select) / [`on_sort`](Table::on_sort);
//! the owning view keeps the state. This keeps domain types out of the library.
//!
//! ```ignore
//! Table::new("files", vec![
//!         Column::new("Name").flex(),
//!         Column::new("Size").width(px(90.)).align_end().sortable(),
//!     ])
//!     .row_count(entries.len())
//!     .selected(self.selected)
//!     .sort(Some((1, false)))
//!     .on_select(cx.listener(|this, ix: &usize, _, cx| { this.selected = Some(*ix); cx.notify() }))
//!     .render_row(move |ix, _window, _cx| vec![
//!         div().child(name_of(ix)).into_any_element(),
//!         div().child(size_of(ix)).into_any_element(),
//!     ])
//! ```

use std::rc::Rc;

use gpui::{div, prelude::*, uniform_list, App, Pixels, SharedString, Styled, Window};

use crate::theme::ActiveTheme;

/// How a [`Column`] is sized.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum ColumnWidth {
    /// Grow to share leftover space equally with other flex columns.
    #[default]
    Flex,
    /// A fixed pixel width.
    Fixed(Pixels),
}

/// Horizontal alignment of a column's content.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ColumnAlign {
    /// Left-aligned (default).
    #[default]
    Start,
    /// Right-aligned (numbers, sizes).
    End,
}

/// A column definition: title, sizing, alignment and sortability.
#[derive(Clone)]
pub struct Column {
    title: SharedString,
    width: ColumnWidth,
    align: ColumnAlign,
    sortable: bool,
}

impl Column {
    /// Create a flexible, left-aligned, non-sortable column.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            width: ColumnWidth::default(),
            align: ColumnAlign::default(),
            sortable: false,
        }
    }

    /// Give the column a fixed pixel width.
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = ColumnWidth::Fixed(width);
        self
    }

    /// Let the column flex to share leftover space (the default).
    pub fn flex(mut self) -> Self {
        self.width = ColumnWidth::Flex;
        self
    }

    /// Right-align the column's content.
    pub fn align_end(mut self) -> Self {
        self.align = ColumnAlign::End;
        self
    }

    /// Mark the column as sortable (header becomes clickable, shows a caret).
    pub fn sortable(mut self) -> Self {
        self.sortable = true;
        self
    }
}

/// Apply a column's width + alignment to a cell `div`.
fn cell_layout<E: Styled>(el: E, column: &Column, align: ColumnAlign) -> E {
    let el = match column.width {
        ColumnWidth::Fixed(w) => el.w(w).flex_shrink_0(),
        ColumnWidth::Flex => el.flex_1().min_w_0(),
    };
    match align {
        ColumnAlign::Start => el.justify_start(),
        ColumnAlign::End => el.justify_end(),
    }
}

/// A handler invoked with a row or column index.
type IndexHandler = Box<dyn Fn(usize, &mut Window, &mut App) + 'static>;
/// Builds the cells (one [`AnyElement`] per column) for a given row.
type RowRenderer = Rc<dyn Fn(usize, &mut Window, &mut App) -> Vec<gpui::AnyElement> + 'static>;

/// A virtualized, fixed-row-height data table.
#[derive(IntoElement)]
pub struct Table {
    id: SharedString,
    columns: Rc<Vec<Column>>,
    row_count: usize,
    row_height: Option<Pixels>,
    selected: Option<usize>,
    sort: Option<(usize, bool)>,
    on_select: Option<Rc<IndexHandler>>,
    on_sort: Option<Rc<IndexHandler>>,
    render_row: Option<RowRenderer>,
}

impl Table {
    /// Create a table with a stable `id` and column definitions.
    pub fn new(id: impl Into<SharedString>, columns: Vec<Column>) -> Self {
        Self {
            id: id.into(),
            columns: Rc::new(columns),
            row_count: 0,
            row_height: None,
            selected: None,
            sort: None,
            on_select: None,
            on_sort: None,
            render_row: None,
        }
    }

    /// Set the number of rows.
    pub fn row_count(mut self, row_count: usize) -> Self {
        self.row_count = row_count;
        self
    }

    /// Override the row height (defaults to the theme's `row_height`).
    pub fn row_height(mut self, height: Pixels) -> Self {
        self.row_height = Some(height);
        self
    }

    /// Mark the currently selected row.
    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    /// Set the active sort as `(column_index, ascending)`, to draw the caret.
    pub fn sort(mut self, sort: Option<(usize, bool)>) -> Self {
        self.sort = sort;
        self
    }

    /// Handler invoked with the index of a clicked row.
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(Box::new(handler)));
        self
    }

    /// Handler invoked with the index of a clicked sortable column header.
    pub fn on_sort(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_sort = Some(Rc::new(Box::new(handler)));
        self
    }

    /// Supply the per-row cell renderer (one element per column).
    pub fn render_row(
        mut self,
        renderer: impl Fn(usize, &mut Window, &mut App) -> Vec<gpui::AnyElement> + 'static,
    ) -> Self {
        self.render_row = Some(Rc::new(renderer));
        self
    }
}

impl RenderOnce for Table {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let row_height = self.row_height.unwrap_or(theme.row_height);
        let columns = self.columns.clone();
        let sort = self.sort;

        // --- Header ---
        let on_sort = self.on_sort.clone();
        let header_cells = columns.iter().enumerate().map(|(ix, column)| {
            let sorted = sort.map(|(c, _)| c == ix).unwrap_or(false);
            let caret = match sort {
                Some((c, asc)) if c == ix => Some(if asc { "▲" } else { "▼" }),
                _ => None,
            };
            let color = if sorted {
                theme.text_muted
            } else {
                theme.text_faint
            };
            let on_sort = on_sort.clone();

            let cell = div()
                .id(ix)
                .flex()
                .items_center()
                .gap_1()
                .h_full()
                .px_2p5()
                .text_color(color)
                .child(column.title.clone())
                .when_some(caret, |this, caret| {
                    this.child(div().text_xs().child(caret))
                });
            let cell = cell_layout(cell, column, column.align);

            if column.sortable {
                cell.cursor_pointer()
                    .hover(|s| s.text_color(theme.text))
                    .when_some(on_sort, |this, on_sort| {
                        this.on_click(move |_, window, cx| on_sort(ix, window, cx))
                    })
                    .into_any_element()
            } else {
                cell.into_any_element()
            }
        });

        let header = div()
            .id("table-head")
            .flex()
            .items_center()
            .h(gpui::px(28.))
            .border_b_1()
            .border_color(theme.border_soft)
            .text_xs()
            .children(header_cells);

        // --- Body (virtualized) ---
        let columns_for_rows = columns.clone();
        let render_row = self.render_row.clone();
        let on_select = self.on_select.clone();
        let selected = self.selected;
        let row_count = self.row_count;

        // Token snapshot so the `'static` row closure doesn't borrow `cx`.
        let bg_hover = theme.bg_hover;
        let bg_selected = theme.bg_selected;
        let text = theme.text;

        let list = uniform_list("table-rows", row_count, move |range, window, cx| {
            let mut rows = Vec::with_capacity(range.len());
            for ix in range {
                let is_selected = selected == Some(ix);
                let cells = render_row
                    .as_ref()
                    .map(|r| r(ix, window, cx))
                    .unwrap_or_default();

                let laid_out = cells.into_iter().enumerate().map(|(c, cell)| {
                    let column = &columns_for_rows[c];
                    cell_layout(
                        div()
                            .flex()
                            .items_center()
                            .h_full()
                            .px_2p5()
                            .overflow_hidden()
                            .child(cell),
                        column,
                        column.align,
                    )
                });

                let on_select = on_select.clone();
                rows.push(
                    div()
                        .id(ix)
                        .flex()
                        .items_center()
                        .h(row_height)
                        .text_color(text)
                        .when(is_selected, |this| this.bg(bg_selected))
                        .when(!is_selected, |this| this.hover(move |s| s.bg(bg_hover)))
                        .when_some(on_select, |this, on_select| {
                            this.cursor_pointer()
                                .on_click(move |_, window, cx| on_select(ix, window, cx))
                        })
                        .children(laid_out),
                );
            }
            rows
        })
        .flex_1();

        div()
            .id(self.id)
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(list)
    }
}
