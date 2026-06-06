//! `Select` — a single-select dropdown (a trigger showing the current value that
//! opens a floating list of options).
//!
//! Stateless, like [`Segmented`](crate::Segmented): the caller owns both the
//! selected index *and* the open/closed flag, and is told about interaction via
//! [`on_toggle`](Select::on_toggle) (trigger clicked / dismissed) and
//! [`on_select`](Select::on_select) (an option chosen). Generic — options carry
//! plain labels, no domain types.
//!
//! The open list is rendered through [`gpui::deferred`] + [`gpui::anchored`], so
//! it floats above clipping/scroll containers (e.g. a modal body) and pins just
//! below the trigger. Dismissal on an outside click is handled internally via
//! `on_mouse_down_out`, which calls `on_toggle`; the trigger only opens (it has
//! no click handler while open), so an outside click can't immediately reopen.
//!
//! ```ignore
//! Select::new("theme")
//!     .option("One Dark")
//!     .option("GitHub Dark")
//!     .selected(self.theme_ix)
//!     .open(self.theme_open)
//!     .on_toggle(cx.listener(|this, _, _, cx| { this.theme_open = !this.theme_open; cx.notify(); }))
//!     .on_select(cx.listener(|this, ix: &usize, _, cx| {
//!         this.theme_ix = *ix;
//!         this.theme_open = false;
//!         cx.notify();
//!     }))
//! ```

use std::rc::Rc;

use gpui::{anchored, deferred, div, point, prelude::*, px, Anchor, App, SharedString, Window};

use crate::theme::ActiveTheme;

/// A handler invoked when the trigger is clicked or the open list is dismissed.
type ToggleHandler = Box<dyn Fn(&mut Window, &mut App) + 'static>;
/// A handler invoked with the chosen option index.
type SelectHandler = Box<dyn Fn(usize, &mut Window, &mut App) + 'static>;

/// A single-select dropdown.
#[derive(IntoElement)]
pub struct Select {
    id: SharedString,
    options: Vec<SharedString>,
    selected: usize,
    open: bool,
    placeholder: SharedString,
    on_toggle: Option<ToggleHandler>,
    on_select: Option<SelectHandler>,
}

impl Select {
    /// Create an empty dropdown with a stable `id`.
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            options: Vec::new(),
            selected: 0,
            open: false,
            placeholder: "Select…".into(),
            on_toggle: None,
            on_select: None,
        }
    }

    /// Append an option with the given `label`.
    pub fn option(mut self, label: impl Into<SharedString>) -> Self {
        self.options.push(label.into());
        self
    }

    /// Set the selected option index.
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Whether the option list is open.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Text shown on the trigger when `selected` is out of range (no selection).
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Handler invoked when the trigger is clicked or the open list is dismissed
    /// by an outside click. The caller flips its open flag.
    pub fn on_toggle(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Box::new(handler));
        self
    }

    /// Handler invoked with the index of a chosen option. The caller typically
    /// records the value *and* closes the list.
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Select {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let open = self.open;
        let selected = self.selected;
        let on_toggle = self.on_toggle.map(Rc::new);
        let on_select = self.on_select.map(Rc::new);

        let current = self
            .options
            .get(selected)
            .cloned()
            .unwrap_or_else(|| self.placeholder.clone());

        // The trigger box. It only opens the list on click: while open, dismissal
        // is owned by the list's `on_mouse_down_out`, so the trigger carries no
        // click handler (which would otherwise immediately reopen it).
        let trigger = div()
            .id(self.id.clone())
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(32.))
            .px_2p5()
            .rounded(theme.radius)
            .bg(theme.bg_input)
            .border_1()
            .border_color(if open {
                theme.border_strong
            } else {
                theme.border
            })
            .text_sm()
            .text_color(theme.text)
            .cursor_pointer()
            .child(div().flex_1().child(current))
            .child(div().text_xs().text_color(theme.text_faint).child("⌄"))
            .when(!open, |this| {
                this.hover(|s| s.border_color(theme.border_strong))
                    .when_some(on_toggle.clone(), |this, toggle| {
                        this.on_click(move |_, window, cx| toggle(window, cx))
                    })
            });

        // One row per option, mirroring the ContextMenu item style. The selected
        // row reads in the accent color and shows a check.
        let rows = self.options.into_iter().enumerate().map(|(ix, label)| {
            let is_selected = ix == selected;
            let handler = on_select.clone();
            div()
                .id(ix)
                .flex()
                .items_center()
                .gap_2p5()
                .px_2p5()
                .py_1p5()
                .rounded(px(4.))
                .text_sm()
                .text_color(if is_selected {
                    theme.accent
                } else {
                    theme.text
                })
                .cursor_pointer()
                .hover(move |s| s.bg(theme.accent).text_color(gpui::white()))
                .child(div().flex_1().child(label))
                .when(is_selected, |this| this.child(div().text_xs().child("✓")))
                .when_some(handler, |this, handler| {
                    this.on_click(move |_, window, cx| handler(ix, window, cx))
                })
        });

        let list = div()
            .occlude()
            .flex()
            .flex_col()
            .min_w(px(180.))
            .p_1()
            .bg(theme.bg_elevated)
            .border_1()
            .border_color(theme.border_strong)
            .rounded(px(7.))
            .shadow_lg()
            .when_some(on_toggle, |this, toggle| {
                this.on_mouse_down_out(move |_, window, cx| toggle(window, cx))
            })
            .children(rows);

        div().relative().w_full().child(trigger).when(open, |this| {
            this.child(deferred(
                anchored()
                    .anchor(Anchor::TopLeft)
                    // Drop the list just below the 32px trigger (+4px gap).
                    .offset(point(px(0.), px(36.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(list),
            ))
        })
    }
}
