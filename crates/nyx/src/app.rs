// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The root view: the app-shell grid, view routing, and global overlays
//! (tweaks modal + toasts). [`AppState`] is the single root entity; this file
//! is its `Render` impl.

use std::time::Duration;

use gpui::{
    actions, anchored, deferred, div, percentage, prelude::*, px, Animation, AnimationExt, Context,
    FontWeight, MouseButton, Transformation, Window,
};
use nyx_ui::{
    ActiveTheme, Button, ButtonVariant, ContextMenu, ContextMenuItem, Modal, Segmented, Theme,
    Toast, Toggle,
};

use crate::assets::{FONT_MONO, FONT_UI};
use crate::icon::icon;
use crate::state::models::Density;
use crate::state::{AppState, View};
use crate::views;

actions!(
    nyx_app,
    [
        /// Open the connection editor in Create mode (the ⌘N shortcut shown on
        /// the welcome screen).
        NewConnection,
    ]
);

/// Register the app-wide keyboard shortcuts (global, no key context). Call once
/// at startup.
pub fn bind_keys(cx: &mut gpui::App) {
    cx.bind_keys([gpui::KeyBinding::new("cmd-n", NewConnection, None)]);
}

impl Render for AppState {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();

        let sidebar = self.sidebar_open.then(|| views::sidebar::render(self, cx));

        let body: gpui::AnyElement = match self.view {
            View::Welcome => views::welcome::render(self, cx).into_any_element(),
            View::Browse => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(views::browser::render(self, cx))
                .child(views::transfer_dock::render(self, cx))
                .into_any_element(),
        };

        let main_col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .bg(theme.bg_app)
            .border_l_1()
            .border_color(theme.border_soft)
            .child(body);

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .font_family(FONT_UI)
            .bg(theme.bg_panel_2)
            .text_color(theme.text)
            .text_sm()
            // Blur on click-away: GPUI keeps focus until another focusable
            // element takes it, so a click on empty chrome would otherwise leave
            // an input focused. This runs in the capture phase (root first), so a
            // click that lands on a field still focuses it on the way down — but a
            // click that hits nothing focusable drops focus (plan M6 D1).
            .capture_any_mouse_down(|_, window, _| window.blur())
            // ⌘N opens the connection editor (the welcome screen advertises it).
            .on_action(cx.listener(|this, _: &NewConnection, _, cx| {
                this.open_editor_create(cx);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when_some(sidebar, |this, sidebar| this.child(sidebar))
                    .child(main_col),
            )
            .child(views::status_bar::render(self, cx))
            .when(self.tweaks_open, |this| {
                let modal = tweaks_modal(self, cx);
                this.child(modal)
            })
            // Backend-driven overlays (M2): the connecting indicator sits under
            // the prompts, which are mutually exclusive in practice.
            .when(
                self.connecting_id.is_some()
                    && self.host_key_prompt.is_none()
                    && self.password_prompt.is_none(),
                |this| this.child(connecting_overlay(self, cx)),
            )
            .when(self.editor.is_some(), |this| {
                this.child(views::connection_editor::render(self, cx))
            })
            .when(self.password_prompt.is_some(), |this| {
                this.child(password_modal(self, cx))
            })
            .when(self.host_key_prompt.is_some(), |this| {
                this.child(host_key_modal(self, cx))
            })
            .when(self.delete_confirm.is_some(), |this| {
                this.child(delete_confirm_modal(self, cx))
            })
            .when(self.file_delete.is_some(), |this| {
                this.child(file_delete_modal(self, cx))
            })
            .when(self.input_prompt.is_some(), |this| {
                this.child(input_prompt_modal(self, cx))
            })
            .when(self.row_menu.is_some(), |this| {
                this.child(row_context_menu(self, cx))
            })
            .when(self.file_menu.is_some(), |this| {
                this.child(file_context_menu(self, cx))
            })
            .when_some(self.toast.as_ref(), |this, toast| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_end()
                        .justify_end()
                        .p_4()
                        .child(Toast::new(toast.message.clone()).variant(toast.variant)),
                )
            })
    }
}

/// The password prompt shown before a connection is attempted (M2). M3 replaces
/// this with a keyring lookup that only prompts on a miss.
fn password_modal(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();
    // Caller guards `password_prompt.is_some()`.
    let prompt = state.password_prompt.as_ref().expect("password prompt set");

    Modal::new("password")
        .title(format!("Connect to {}", prompt.profile_name))
        .width(px(420.))
        .on_close({
            let view = view.clone();
            move |_window, cx| {
                view.update(cx, |this, cx| {
                    this.cancel_password();
                    cx.notify();
                });
            }
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .font_family(FONT_MONO)
                        .text_color(theme.text_faint)
                        .child(prompt.host_label.clone()),
                )
                .child(prompt.input.clone())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.text_muted)
                                .child("Save to keychain"),
                        )
                        .child(Toggle::new("pw-save", prompt.save_to_keychain).on_change(
                            cx.listener(|this, on, _window, cx| {
                                this.set_password_save(*on);
                                cx.notify();
                            }),
                        )),
                ),
        )
        .footer(
            div()
                .flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("pw-cancel", "Cancel")
                        .variant(ButtonVariant::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_password();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("pw-connect", "Connect")
                        .variant(ButtonVariant::Primary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_password(cx);
                            cx.notify();
                        })),
                ),
        )
}

/// The host-key trust-on-first-use prompt (an unknown key was presented). A
/// mismatch never reaches here — it is rejected outright and surfaced as a toast.
fn host_key_modal(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();
    let prompt = state.host_key_prompt.as_ref().expect("host-key prompt set");

    Modal::new("host-key")
        .title("Verify host key")
        .width(px(470.))
        .on_close({
            // Dismissing the prompt is a rejection.
            let view = view.clone();
            move |_window, cx| {
                view.update(cx, |this, cx| {
                    this.reject_host_key();
                    cx.notify();
                });
            }
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(div().text_sm().text_color(theme.text_muted).child(format!(
                    "The authenticity of host “{}” can't be established.",
                    prompt.host
                )))
                .child(
                    div()
                        .p_3()
                        .rounded(theme.radius)
                        .bg(theme.bg_input)
                        .border_1()
                        .border_color(theme.border)
                        .font_family(FONT_MONO)
                        .text_xs()
                        .text_color(theme.text)
                        .child(prompt.fingerprint.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.text_faint)
                        .child("Trust this key and continue? It will be saved to known_hosts."),
                ),
        )
        .footer(
            div()
                .flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("hk-reject", "Reject")
                        .variant(ButtonVariant::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reject_host_key();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("hk-trust", "Trust & connect")
                        .variant(ButtonVariant::Primary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.trust_host_key();
                            cx.notify();
                        })),
                ),
        )
}

/// The "remove connection?" confirmation (deletes the profile + its keychain
/// entry). The reusable confirm-destructive pattern M4 will share for file ops.
fn delete_confirm_modal(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();
    let confirm = state.delete_confirm.as_ref().expect("delete confirm set");

    Modal::new("delete-confirm")
        .title("Remove connection")
        .width(px(400.))
        .on_close({
            let view = view.clone();
            move |_window, cx| {
                view.update(cx, |this, cx| {
                    this.cancel_delete();
                    cx.notify();
                });
            }
        })
        .child(div().text_sm().text_color(theme.text_muted).child(format!(
            "Remove “{}”? This deletes the saved profile and its keychain password — this can't be undone.",
            confirm.profile_name
        )))
        .footer(
            div()
                .flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("del-cancel", "Cancel")
                        .variant(ButtonVariant::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_delete();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("del-remove", "Remove")
                        .variant(ButtonVariant::Danger)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_delete(cx);
                            cx.notify();
                        })),
                ),
        )
}

/// The sidebar row right-click menu (Edit / Remove), anchored at the cursor.
///
/// A full-screen backdrop dismisses the menu on an outside click; the menu
/// surface `occlude()`s its own region so a click on an item lands on the item.
fn row_context_menu(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let menu = state.row_menu.as_ref().expect("row menu set");
    let position = menu.position;
    let edit_id = menu.profile_id.clone();
    let remove_id = menu.profile_id.clone();
    let remove_name = menu.profile_name.clone();

    let surface = ContextMenu::new("row-ctx")
        .item(
            ContextMenuItem::new("ctx-edit", "Edit").on_click(cx.listener(
                move |this, _, _, cx| {
                    this.open_editor_edit(&edit_id, cx);
                    cx.notify();
                },
            )),
        )
        .separator()
        .item(
            ContextMenuItem::new("ctx-remove", "Remove")
                .danger()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_delete_confirm(remove_id.clone(), remove_name.clone());
                    cx.notify();
                })),
        );

    div()
        .absolute()
        .inset_0()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.close_row_menu();
                cx.notify();
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, _, _, cx| {
                this.close_row_menu();
                cx.notify();
            }),
        )
        .child(deferred(
            anchored()
                .position(position)
                .child(div().occlude().child(surface)),
        ))
}

/// The browser file-row right-click menu (Download / Rename / Delete / Copy
/// path), anchored at the cursor — modeled on [`row_context_menu`]. Download and
/// Delete act on the whole selection; Rename and Copy path on the clicked row.
fn file_context_menu(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let menu = state.file_menu.as_ref().expect("file menu set");
    let position = menu.position;
    // Directory download is post-MVP (D5): disable Download on a folder row.
    let download_disabled = menu.is_dir;

    let surface = ContextMenu::new("file-ctx")
        .item(
            ContextMenuItem::new("file-download", "Download")
                .disabled(download_disabled)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.download_selection(cx);
                    cx.notify();
                })),
        )
        .item(
            ContextMenuItem::new("file-rename", "Rename").on_click(cx.listener(
                |this, _, _, cx| {
                    this.start_rename(cx);
                    cx.notify();
                },
            )),
        )
        .item(
            ContextMenuItem::new("file-copy-path", "Copy path").on_click(cx.listener(
                |this, _, _, cx| {
                    this.copy_path(cx);
                    cx.notify();
                },
            )),
        )
        .separator()
        .item(
            ContextMenuItem::new("file-delete", "Delete")
                .danger()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_delete(cx);
                    cx.notify();
                })),
        );

    div()
        .absolute()
        .inset_0()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.close_file_menu();
                cx.notify();
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, _, _, cx| {
                this.close_file_menu();
                cx.notify();
            }),
        )
        .child(deferred(
            anchored()
                .position(position)
                .child(div().occlude().child(surface)),
        ))
}

/// The reusable single-field input modal (New folder / Rename), modeled on
/// [`password_modal`]. Submitting validates non-empty + no `/` in the state.
fn input_prompt_modal(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();
    let prompt = state.input_prompt.as_ref().expect("input prompt set");
    let title = prompt.title.clone();
    let label = prompt.label.clone();
    let submit_label = prompt.submit_label.clone();

    Modal::new("input-prompt")
        .title(title)
        .width(px(400.))
        .on_close({
            let view = view.clone();
            move |_window, cx| {
                view.update(cx, |this, cx| {
                    this.cancel_input();
                    cx.notify();
                });
            }
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_muted)
                        .child(label),
                )
                .child(prompt.input.clone()),
        )
        .footer(
            div()
                .flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("input-cancel", "Cancel")
                        .variant(ButtonVariant::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_input();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("input-submit", submit_label)
                        .variant(ButtonVariant::Primary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.submit_input(cx);
                            cx.notify();
                        })),
                ),
        )
}

/// The file-delete confirmation (one or many entries), modeled on
/// [`delete_confirm_modal`] — danger button, copy that names the count and warns
/// the delete is recursive for folders and can't be undone (plan D8).
fn file_delete_modal(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();
    let confirm = state.file_delete.as_ref().expect("file delete set");
    let count = confirm.entries.len();
    let has_dir = confirm.entries.iter().any(|(_, is_dir)| *is_dir);

    let summary = if count == 1 {
        format!("Delete “{}”?", confirm.entries[0].0)
    } else {
        format!("Delete {count} items?")
    };
    let detail = if has_dir {
        "Folders are deleted recursively. This can't be undone."
    } else {
        "This can't be undone."
    };

    Modal::new("file-delete-confirm")
        .title("Delete")
        .width(px(400.))
        .on_close({
            let view = view.clone();
            move |_window, cx| {
                view.update(cx, |this, cx| {
                    this.cancel_file_delete();
                    cx.notify();
                });
            }
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().text_color(theme.text).child(summary))
                .child(div().text_xs().text_color(theme.text_faint).child(detail)),
        )
        .footer(
            div()
                .flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("file-del-cancel", "Cancel")
                        .variant(ButtonVariant::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_file_delete();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("file-del-remove", "Delete")
                        .variant(ButtonVariant::Danger)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_file_delete(cx);
                            cx.notify();
                        })),
                ),
        )
}

/// A lightweight "connecting…" overlay (full spinner polish is M6).
fn connecting_overlay(state: &AppState, cx: &Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let name = state
        .connecting_id
        .as_deref()
        .and_then(|id| state.connections.iter().find(|c| c.profile.id == id))
        .map(|c| c.profile.name.clone())
        .unwrap_or_default();

    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::black().opacity(0.55))
        .occlude()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .px_8()
                .py_6()
                .rounded(px(10.))
                .bg(theme.bg_elevated)
                .border_1()
                .border_color(theme.border_strong)
                .shadow_lg()
                .child(
                    // A continuously rotating spinner (matches the design's
                    // `.connecting .spin` ring) instead of a static glyph.
                    icon("refresh", 26., theme.accent).with_animation(
                        "connecting-spinner",
                        Animation::new(Duration::from_secs(1)).repeat(),
                        |icon, delta| {
                            icon.with_transformation(Transformation::rotate(percentage(delta)))
                        },
                    ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.text)
                        .child(format!("Connecting to {name}…")),
                ),
        )
}

/// The in-memory tweaks modal (density, permissions column, theme).
fn tweaks_modal(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let density_ix = state.density.index();
    let show_perms = state.show_perms;
    let theme_ix = if cx.theme().name == "One Dark" { 0 } else { 1 };
    let view = cx.entity();

    Modal::new("tweaks")
        .title("Tweaks")
        .width(px(420.))
        .on_close({
            let view = view.clone();
            move |_window, cx| {
                view.update(cx, |this, cx| {
                    this.tweaks_open = false;
                    cx.notify();
                });
            }
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(field(
                    "Color scheme",
                    Segmented::new("tw-theme")
                        .segment("One Dark")
                        .segment("GitHub Dark")
                        .selected(theme_ix)
                        .on_select({
                            let view = view.clone();
                            move |ix, _window, cx| {
                                let next = if ix == 0 {
                                    Theme::one_dark()
                                } else {
                                    Theme::github_dark()
                                };
                                cx.set_global(next);
                                view.update(cx, |_, cx| cx.notify());
                            }
                        }),
                    cx,
                ))
                .child(field(
                    "Row density",
                    Segmented::new("tw-density")
                        .segment("Compact")
                        .segment("Comfortable")
                        .segment("Spacious")
                        .selected(density_ix)
                        .on_select({
                            let view = view.clone();
                            move |ix, _window, cx| {
                                view.update(cx, |this, cx| {
                                    this.density = Density::ALL[ix.min(2)];
                                    cx.notify();
                                });
                            }
                        }),
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.text_muted)
                                .child("Permissions column"),
                        )
                        .child(Toggle::new("tw-perms", show_perms).on_change({
                            let view = view.clone();
                            move |on, _window, cx| {
                                let on = *on;
                                view.update(cx, |this, cx| {
                                    this.show_perms = on;
                                    cx.notify();
                                });
                            }
                        })),
                ),
        )
        .footer(
            div().flex().w_full().justify_end().child(
                Button::new("tw-done", "Done")
                    .variant(ButtonVariant::Primary)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tweaks_open = false;
                        cx.notify();
                    })),
            ),
        )
}

fn field(
    label: &'static str,
    control: impl IntoElement,
    cx: &Context<AppState>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().text_muted)
                .child(label),
        )
        .child(control)
}
