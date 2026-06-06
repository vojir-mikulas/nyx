//! `Button` — the reference component for the variant API. Stateless
//! [`RenderOnce`]; an id is required so the click handler can be attached.

use gpui::{div, prelude::*, AnyElement, App, ClickEvent, ElementId, Hsla, SharedString, Window};

use crate::theme::ActiveTheme;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonVariant {
    #[default]
    Primary,
    Secondary,
    Ghost,
    Danger,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonSize {
    Sm,
    #[default]
    Md,
}

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

    /// Leading icon — any `impl IntoElement`, never an icon enum (domain-free).
    pub fn icon(mut self, icon: impl IntoElement) -> Self {
        self.icon = Some(icon.into_any_element());
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

/// `(background, foreground, border, hover bg)` for a variant.
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
