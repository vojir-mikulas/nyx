//! `Table` — a virtualized, fixed-row-height data table on GPUI's
//! [`uniform_list`](gpui::uniform_list). Fully generic and stateless: the caller
//! declares [`Column`]s + a row renderer and owns selection/sort, which the table
//! renders and reports clicks against.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    div, prelude::*, uniform_list, App, ClickEvent, ExternalPaths, MouseButton, Pixels, Point,
    SharedString, Styled, Window,
};

use crate::theme::ActiveTheme;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum ColumnWidth {
    /// Shares leftover space equally with other flex columns.
    #[default]
    Flex,
    Fixed(Pixels),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ColumnAlign {
    #[default]
    Start,
    End,
}

#[derive(Clone)]
pub struct Column {
    title: SharedString,
    width: ColumnWidth,
    align: ColumnAlign,
    sortable: bool,
}

impl Column {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            width: ColumnWidth::default(),
            align: ColumnAlign::default(),
            sortable: false,
        }
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = ColumnWidth::Fixed(width);
        self
    }

    pub fn flex(mut self) -> Self {
        self.width = ColumnWidth::Flex;
        self
    }

    pub fn align_end(mut self) -> Self {
        self.align = ColumnAlign::End;
        self
    }

    pub fn sortable(mut self) -> Self {
        self.sortable = true;
        self
    }
}

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

type IndexHandler = Box<dyn Fn(usize, &mut Window, &mut App) + 'static>;
/// Row-click handler; also receives the click, for modifier-aware selection.
type RowClickHandler = Box<dyn Fn(usize, &ClickEvent, &mut Window, &mut App) + 'static>;
/// Receives the row index and cursor position, to anchor a context menu.
type RowSecondaryHandler = Box<dyn Fn(usize, Point<Pixels>, &mut Window, &mut App) + 'static>;
/// Builds one cell [`AnyElement`] per column for a row.
type RowRenderer = Rc<dyn Fn(usize, &mut Window, &mut App) -> Vec<gpui::AnyElement> + 'static>;
/// Builds the sort caret. Returns an [`AnyElement`] so the library stays
/// domain- and icon-set-free.
type CaretBuilder = Rc<dyn Fn() -> gpui::AnyElement + 'static>;
type RowDropHandler = Rc<dyn Fn(usize, &ExternalPaths, &mut Window, &mut App) + 'static>;
/// Fired when a drag-*out* gesture starts on a row (the owner anchors a native
/// OS drag to the window). Domain-agnostic: the table only reports the row.
type RowDragOutHandler = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

/// The (invisible) in-app drag preview. The visible drag image is owned by the
/// native OS drag the row handler starts, so GPUI's own preview is empty.
struct DragPreview;

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
}

#[derive(IntoElement)]
pub struct Table {
    id: SharedString,
    columns: Rc<Vec<Column>>,
    row_count: usize,
    row_height: Option<Pixels>,
    selected: Option<usize>,
    selected_set: Option<Rc<HashSet<usize>>>,
    sort: Option<(usize, bool)>,
    on_select: Option<Rc<RowClickHandler>>,
    on_secondary: Option<Rc<RowSecondaryHandler>>,
    on_activate: Option<Rc<IndexHandler>>,
    on_sort: Option<Rc<IndexHandler>>,
    render_row: Option<RowRenderer>,
    sort_caret_asc: Option<CaretBuilder>,
    sort_caret_desc: Option<CaretBuilder>,
    on_row_drop: Option<RowDropHandler>,
    droppable_rows: Option<Rc<HashSet<usize>>>,
    on_row_drag_out: Option<RowDragOutHandler>,
    draggable_rows: Option<Rc<HashSet<usize>>>,
}

impl Table {
    pub fn new(id: impl Into<SharedString>, columns: Vec<Column>) -> Self {
        Self {
            id: id.into(),
            columns: Rc::new(columns),
            row_count: 0,
            row_height: None,
            selected: None,
            selected_set: None,
            sort: None,
            on_select: None,
            on_secondary: None,
            on_activate: None,
            on_sort: None,
            render_row: None,
            sort_caret_asc: None,
            sort_caret_desc: None,
            on_row_drop: None,
            droppable_rows: None,
            on_row_drag_out: None,
            draggable_rows: None,
        }
    }

    pub fn row_count(mut self, row_count: usize) -> Self {
        self.row_count = row_count;
        self
    }

    /// Defaults to the theme's `row_height`.
    pub fn row_height(mut self, height: Pixels) -> Self {
        self.row_height = Some(height);
        self
    }

    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    /// Multi-selection: a row highlights when in this set *or* equal to
    /// [`selected`](Self::selected), so both APIs compose.
    pub fn selected_set(mut self, selected: HashSet<usize>) -> Self {
        self.selected_set = Some(Rc::new(selected));
        self
    }

    /// `(column_index, ascending)`, to draw the caret.
    pub fn sort(mut self, sort: Option<(usize, bool)>) -> Self {
        self.sort = sort;
        self
    }

    pub fn on_select(
        mut self,
        handler: impl Fn(usize, &ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(Box::new(handler)));
        self
    }

    pub fn on_secondary(
        mut self,
        handler: impl Fn(usize, Point<Pixels>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_secondary = Some(Rc::new(Box::new(handler)));
        self
    }

    /// Double-click; does not also fire [`on_select`](Self::on_select).
    pub fn on_activate(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_activate = Some(Rc::new(Box::new(handler)));
        self
    }

    pub fn on_sort(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_sort = Some(Rc::new(Box::new(handler)));
        self
    }

    /// Caret glyphs for the active sort column. Unset falls back to built-in
    /// Unicode triangles.
    pub fn sort_carets(
        mut self,
        asc: impl Fn() -> gpui::AnyElement + 'static,
        desc: impl Fn() -> gpui::AnyElement + 'static,
    ) -> Self {
        self.sort_caret_asc = Some(Rc::new(asc));
        self.sort_caret_desc = Some(Rc::new(desc));
        self
    }

    /// Only these rows highlight on drag-over and dispatch
    /// [`on_row_drop`](Self::on_row_drop) (the owner knows which are directories).
    pub fn droppable_rows(mut self, rows: HashSet<usize>) -> Self {
        self.droppable_rows = Some(Rc::new(rows));
        self
    }

    pub fn on_row_drop(
        mut self,
        handler: impl Fn(usize, &ExternalPaths, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_row_drop = Some(Rc::new(handler));
        self
    }

    /// Only these rows start a drag-*out* gesture (the owner knows which are
    /// draggable, e.g. files but not directories).
    pub fn draggable_rows(mut self, rows: HashSet<usize>) -> Self {
        self.draggable_rows = Some(Rc::new(rows));
        self
    }

    /// Called when a drag-out gesture begins on a [`draggable`](Self::draggable_rows)
    /// row. The owner anchors a native OS drag to the window here.
    pub fn on_row_drag_out(
        mut self,
        handler: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_row_drag_out = Some(Rc::new(handler));
        self
    }

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

        let on_sort = self.on_sort.clone();
        let caret_asc = self.sort_caret_asc.clone();
        let caret_desc = self.sort_caret_desc.clone();
        let header_cells = columns.iter().enumerate().map(|(ix, column)| {
            let sorted = sort.map(|(c, _)| c == ix).unwrap_or(false);
            // Caller-supplied caret glyph if set, else the built-in triangle.
            let caret: Option<gpui::AnyElement> = match sort {
                Some((c, asc)) if c == ix => Some(if asc {
                    caret_asc
                        .as_ref()
                        .map(|f| f())
                        .unwrap_or_else(|| div().text_xs().child("▲").into_any_element())
                } else {
                    caret_desc
                        .as_ref()
                        .map(|f| f())
                        .unwrap_or_else(|| div().text_xs().child("▼").into_any_element())
                }),
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
                .when_some(caret, |this, caret| this.child(caret));
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

        let columns_for_rows = columns.clone();
        let render_row = self.render_row.clone();
        let on_select = self.on_select.clone();
        let on_secondary = self.on_secondary.clone();
        let on_activate = self.on_activate.clone();
        let on_row_drop = self.on_row_drop.clone();
        let droppable_rows = self.droppable_rows.clone();
        let on_row_drag_out = self.on_row_drag_out.clone();
        let draggable_rows = self.draggable_rows.clone();
        let selected = self.selected;
        let selected_set = self.selected_set.clone();
        let row_count = self.row_count;

        // Token snapshot so the `'static` row closure doesn't borrow `cx`.
        let bg_hover = theme.bg_hover;
        let bg_selected = theme.bg_selected;
        let drop_highlight = theme.bg_active;
        let text = theme.text;

        let list = uniform_list("table-rows", row_count, move |range, window, cx| {
            let mut rows = Vec::with_capacity(range.len());
            for ix in range {
                let is_selected =
                    selected == Some(ix) || selected_set.as_ref().is_some_and(|s| s.contains(&ix));
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
                let on_secondary = on_secondary.clone();
                let on_activate = on_activate.clone();
                let on_row_drop = on_row_drop.clone();
                let on_row_drag_out = on_row_drag_out.clone();
                let clickable = on_select.is_some() || on_activate.is_some();
                let is_droppable = droppable_rows.as_ref().is_some_and(|s| s.contains(&ix))
                    && on_row_drop.is_some();
                let is_draggable_out = draggable_rows.as_ref().is_some_and(|s| s.contains(&ix))
                    && on_row_drag_out.is_some();
                rows.push(
                    div()
                        .id(ix)
                        .flex()
                        .items_center()
                        // `uniform_list` lays each row out as a layout root; `w_full`
                        // makes it fill the width so flex columns align with the header.
                        .w_full()
                        .h(row_height)
                        .text_color(text)
                        .when(is_selected, |this| this.bg(bg_selected))
                        .when(!is_selected, |this| this.hover(move |s| s.bg(bg_hover)))
                        .when(clickable || on_secondary.is_some(), |this| {
                            this.cursor_pointer()
                        })
                        .when(clickable, |this| {
                            this.on_click(move |event, window, cx| {
                                if event.click_count() >= 2 {
                                    if let Some(on_activate) = on_activate.as_ref() {
                                        on_activate(ix, window, cx);
                                        return;
                                    }
                                }
                                if let Some(on_select) = on_select.as_ref() {
                                    on_select(ix, event, window, cx);
                                }
                            })
                        })
                        .when_some(on_secondary, |this, on_secondary| {
                            this.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                                on_secondary(ix, event.position, window, cx);
                            })
                        })
                        .when(is_droppable, |this| {
                            let this = this
                                .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_highlight));
                            this.when_some(on_row_drop, |this, on_row_drop| {
                                this.on_drop::<ExternalPaths>(move |paths, window, cx| {
                                    on_row_drop(ix, paths, window, cx);
                                })
                            })
                        })
                        // Drag a file row out to the OS file manager. GPUI's
                        // `on_drag` is the gesture detector; the row handler starts
                        // the real (native) drag and we hand GPUI an empty preview.
                        .when(is_draggable_out, |this| {
                            this.when_some(on_row_drag_out, |this, on_row_drag_out| {
                                this.on_drag(ix, move |_ix, _offset, window, cx| {
                                    on_row_drag_out(ix, window, cx);
                                    cx.new(|_| DragPreview)
                                })
                            })
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
