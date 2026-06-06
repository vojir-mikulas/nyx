//! `Button` — the reference component demonstrating the variant API.
//!
//! Styling is a function of typed props (`variant`, `size`, `disabled`) — the
//! `cva` analog. It is a stateless [`RenderOnce`] element; an id is required so
//! the click handler can be attached.

use gpui::{div, prelude::*, AnyElement, App, ClickEvent, ElementId, Hsla, SharedString, Window};

use crate::theme::ActiveTheme;

/// A boxed click handler, in GPUI's `(event, window, app)` shape.
type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Visual emphasis of a [`Button`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonVariant {
    /// Filled accent button — the primary action.
    #[default]
    Primary,
    /// Neutral, bordered button — secondary actions.
    Secondary,
    /// Borderless, low-emphasis button.
    Ghost,
    /// Destructive action.
    Danger,
}

/// Size of a [`Button`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonSize {
    /// Compact (toolbars, inline).
    Sm,
    /// Default size.
    #[default]
    Md,
}

/// A themed, clickable button.
///
/// ```ignore
/// Button::new("connect", "Connect")
///     .variant(ButtonVariant::Primary)
///     .size(ButtonSize::Sm)
///     .on_click(|_, _, _| { /* … */ });
/// ```
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    icon: Option<AnyElement>,
    variant: ButtonVariant,
    size: ButtonSize,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    /// Create a button with a stable `id` and a `label`.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            disabled: false,
            on_click: None,
        }
    }

    /// Set an optional leading icon element (an `svg()`, glyph, etc.). Stays
    /// domain-free: any `impl IntoElement`, never an icon enum.
    pub fn icon(mut self, icon: impl IntoElement) -> Self {
        self.icon = Some(icon.into_any_element());
        self
    }

    /// Set the visual variant.
    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Set the size.
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
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

/// Resolved colors for a variant: `(background, foreground, border, hover bg)`.
fn variant_colors(variant: ButtonVariant, theme: &crate::Theme) -> (Hsla, Hsla, Hsla, Hsla) {
    match variant {
        ButtonVariant::Primary => (
            theme.accent,
            theme.on_accent,
            theme.accent,
            theme.accent_hover,
        ),
        ButtonVariant::Secondary => (theme.bg_active, theme.text, theme.border, theme.bg_hover),
        ButtonVariant::Ghost => (
            gpui::transparent_black(),
            theme.text_muted,
            gpui::transparent_black(),
            theme.bg_hover,
        ),
        ButtonVariant::Danger => (theme.bg_active, theme.red, theme.border, theme.bg_hover),
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, border, hover_bg) = variant_colors(self.variant, theme);

        let height = match self.size {
            ButtonSize::Sm => 24.0,
            ButtonSize::Md => 32.0,
        };

        let base = div()
            .id(self.id)
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .h(gpui::px(height))
            .px_3()
            .rounded_md()
            .text_sm()
            .bg(bg)
            .text_color(fg)
            .border_1()
            .border_color(border)
            .when_some(self.icon, |this, icon| this.child(icon))
            .child(self.label);

        let interactive = if self.disabled {
            base.opacity(0.5)
        } else {
            base.cursor_pointer()
                .hover(move |s| s.bg(hover_bg).border_color(hover_bg))
        };

        match (self.disabled, self.on_click) {
            (false, Some(handler)) => {
                interactive.on_click(move |event, window, cx| handler(event, window, cx))
            }
            _ => interactive,
        }
    }
}
