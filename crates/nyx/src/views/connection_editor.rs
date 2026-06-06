//! The connection editor modal — create or edit a profile.
//!
//! A [`Modal`] wrapping the `nyx-ui` form kit. All mutation goes back through
//! [`AppState`] methods; this file only reads `state.editor` and emits elements.

use gpui::{div, prelude::*, px, Context, FontWeight};
use nyx_core::Protocol;
use nyx_ui::{ActiveTheme, Button, ButtonSize, ButtonVariant, Modal, Segmented};

use crate::state::AppState;

/// Render the editor modal. The caller guards `state.editor.is_some()`.
pub fn render(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();
    let editor = state.editor.as_ref().expect("editor open");

    let protocol_ix = match editor.protocol {
        Protocol::Sftp => 0,
        Protocol::Ftp => 1,
        Protocol::Ftps => 2,
    };
    let color_ix = editor.color.index();
    let is_new = editor.is_new;

    // The inline test-connection status line.
    let status: Option<gpui::AnyElement> = if editor.testing {
        Some(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_xs()
                .text_color(theme.text_muted)
                .child(crate::icon::spinner("ce-testing", 13., theme.text_muted))
                .child("Testing connection…")
                .into_any_element(),
        )
    } else {
        editor.test_status.as_ref().map(|st| {
            let (color, mark) = if st.ok {
                (theme.green, "✓")
            } else {
                (theme.red, "✗")
            };
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_xs()
                .text_color(color)
                .child(mark)
                .child(st.message.clone())
                .into_any_element()
        })
    };

    Modal::new("connection-editor")
        .title(if is_new {
            "New connection"
        } else {
            "Edit connection"
        })
        .width(px(460.))
        .on_close({
            let view = view.clone();
            move |_window, cx| {
                view.update(cx, |this, cx| {
                    this.close_editor();
                    cx.notify();
                });
            }
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3p5()
                .child(field("Name", editor.name.clone(), cx))
                .child(field(
                    "Protocol",
                    Segmented::new("ce-protocol")
                        .segment("SFTP")
                        .segment("FTP")
                        .segment("FTPS")
                        .selected(protocol_ix)
                        .on_select({
                            let view = view.clone();
                            move |ix, _window, cx| {
                                view.update(cx, |this, cx| {
                                    this.set_editor_protocol(ix, cx);
                                    cx.notify();
                                });
                            }
                        }),
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().child(field("Host", editor.host.clone(), cx)))
                        .child(
                            div()
                                .w(px(96.))
                                .child(field("Port", editor.port.clone(), cx)),
                        ),
                )
                .child(field("Username", editor.username.clone(), cx))
                .child(field("Remote path", editor.remote_path.clone(), cx))
                .child(field(
                    "Accent color",
                    Segmented::new("ce-color")
                        .segment("Blue")
                        .segment("Purple")
                        .segment("Green")
                        .selected(color_ix)
                        .on_select({
                            let view = view.clone();
                            move |ix, _window, cx| {
                                view.update(cx, |this, cx| {
                                    this.set_editor_color(ix);
                                    cx.notify();
                                });
                            }
                        }),
                    cx,
                ))
                .child(field("Password", editor.password.clone(), cx))
                .when_some(status, |this, status| {
                    this.child(div().pt_1().child(status))
                }),
        )
        .footer(
            div()
                .flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    Button::new("ce-test", "Test connection")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Sm)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.test_editor_connection(cx);
                            cx.notify();
                        })),
                )
                .child(div().flex_1())
                .when(!is_new, |this| {
                    this.child(
                        Button::new("ce-delete", "Delete")
                            .variant(ButtonVariant::Danger)
                            .size(ButtonSize::Sm)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(editor) = this.editor.as_ref() {
                                    let id = editor.id.clone();
                                    let name = editor.name.read(cx).content();
                                    this.open_delete_confirm(id, name);
                                }
                                cx.notify();
                            })),
                    )
                })
                .child(
                    Button::new("ce-cancel", "Cancel")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Sm)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.close_editor();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("ce-save", "Save")
                        .variant(ButtonVariant::Primary)
                        .size(ButtonSize::Sm)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.save_editor(cx);
                            cx.notify();
                        })),
                ),
        )
}

/// A labelled form field (label above the control).
fn field(
    label: &'static str,
    control: impl IntoElement,
    cx: &Context<AppState>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().text_muted)
                .child(label),
        )
        .child(control)
}
