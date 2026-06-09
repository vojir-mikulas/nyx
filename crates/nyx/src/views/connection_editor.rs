//! The connection editor modal - create or edit a profile.
//!
//! A [`Modal`] wrapping the `flint` form kit. All mutation goes back through
//! [`AppState`] methods; this file only reads `state.editor` and emits elements.

use flint::{ActiveTheme, Button, ButtonSize, ButtonVariant, Modal, Segmented, Theme};
use gpui::{div, prelude::*, px, Context, Entity, FontWeight};
use nyx_core::{FtpsMode, Protocol};

use crate::state::models::AccentKind;
use crate::state::AppState;

/// Render the editor modal. The caller guards `state.editor.is_some()`.
pub fn render(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();
    let Some(editor) = state.editor.as_ref() else {
        return div().into_any_element();
    };

    let protocol_ix = match editor.protocol {
        Protocol::Sftp => 0,
        Protocol::Ftp => 1,
        Protocol::Ftps => 2,
    };
    let is_new = editor.is_new;
    let auth_is_key = editor.auth_is_key;
    let auth_is_anonymous = editor.auth_is_anonymous;
    // Password field shows only when the selected method actually sends one.
    let auth_uses_password = !auth_is_key && !auth_is_anonymous;
    let is_sftp = matches!(editor.protocol, Protocol::Sftp);
    let is_ftp = matches!(editor.protocol, Protocol::Ftp);
    let is_ftps = matches!(editor.protocol, Protocol::Ftps);
    let ftps_implicit = editor.ftps_mode == FtpsMode::Implicit;

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
                .when(is_ftps, |this| {
                    this.child(field(
                        "Encryption",
                        Segmented::new("ce-ftps-mode")
                            .segment("Explicit")
                            .segment("Implicit")
                            .selected(if ftps_implicit { 1 } else { 0 })
                            .on_select({
                                let view = view.clone();
                                move |ix, _window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.set_editor_ftps_mode(ix, cx);
                                        cx.notify();
                                    });
                                }
                            }),
                        cx,
                    ))
                })
                .when(is_ftp, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.red)
                            .child("⚠ FTP is unencrypted - credentials and files cross the network in the clear."),
                    )
                })
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
                .when(!auth_is_anonymous, |this| {
                    this.child(field("Username", editor.username.clone(), cx))
                })
                .child(field("Remote path", editor.remote_path.clone(), cx))
                .child(field(
                    "Accent color",
                    color_picker(editor.color, &theme, &view),
                    cx,
                ))
                // Auth selector: SFTP picks Password/Key; FTP/FTPS picks
                // Password/Anonymous. Index 1 is the protocol's "second" method.
                .child(field(
                    "Authentication",
                    Segmented::new("ce-auth")
                        .segment("Password")
                        .segment(if is_sftp { "Key" } else { "Anonymous" })
                        .selected(if auth_is_key || auth_is_anonymous { 1 } else { 0 })
                        .on_select({
                            let view = view.clone();
                            move |ix, _window, cx| {
                                view.update(cx, |this, cx| {
                                    this.set_editor_auth(ix);
                                    cx.notify();
                                });
                            }
                        }),
                    cx,
                ))
                .when(auth_uses_password, |this| {
                    this.child(field("Password", editor.password.clone(), cx))
                })
                .when(is_sftp && auth_is_key, |this| {
                    this.child(field(
                        "Private key",
                        div()
                            .flex()
                            .gap_2()
                            .child(div().flex_1().child(editor.key_path.clone()))
                            .child(
                                Button::new("ce-key-browse", "Browse")
                                    .variant(ButtonVariant::Secondary)
                                    .size(ButtonSize::Sm)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.pick_key_file(cx);
                                        cx.notify();
                                    })),
                            ),
                        cx,
                    ))
                    .child(field("Passphrase", editor.passphrase.clone(), cx))
                })
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
        .into_any_element()
}

/// A labelled form field (label above the control).
/// A row of clickable color swatches - the connection's accent picker. The
/// selected swatch carries a ring in its own color.
fn color_picker(selected: AccentKind, theme: &Theme, view: &Entity<AppState>) -> impl IntoElement {
    div()
        .flex()
        .gap_2()
        .children(AccentKind::ALL.into_iter().enumerate().map(|(ix, kind)| {
            let c = kind.color(theme);
            let is_sel = kind == selected;
            div()
                .id(("color-swatch", ix))
                .flex()
                .items_center()
                .justify_center()
                .size(px(26.))
                .rounded_full()
                .border_2()
                .border_color(if is_sel { c } else { gpui::transparent_black() })
                .cursor_pointer()
                .child(div().size(px(16.)).rounded_full().bg(c))
                .on_click({
                    let view = view.clone();
                    move |_, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.set_editor_color(ix);
                            cx.notify();
                        });
                    }
                })
        }))
}

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
