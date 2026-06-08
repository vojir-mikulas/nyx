//! The root view: the app-shell grid, view routing, and global overlays
//! (tweaks modal + toasts). [`AppState`] is the single root entity; this file
//! is its `Render` impl.

use gpui::{
    anchored, deferred, div, prelude::*, px, Context, Focusable, FontWeight, MouseButton, Window,
};
use nyx_core::CollisionChoice;
use nyx_ui::{
    ActiveTheme, Button, ButtonVariant, ContextMenu, ContextMenuItem, Modal, Segmented, Select,
    Theme, Toast, Toggle,
};

use crate::assets::{FONT_MONO, FONT_UI};
use crate::keymap::{
    CloseTab, Dismiss, FocusFilter, FocusNext, FocusPrev, NewConnection, OpenSettings, Quit,
    Refresh, ShowShortcuts, ToggleSidebar,
};
use crate::state::models::Density;
use crate::state::{AppState, View};
use crate::views;

impl Render for AppState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep something focused so global keys and modal Enter/Esc dispatch: honor
        // a queued focus target (modal autofocus), else fall back to a sensible
        // default when focus was lost (e.g. after a click on empty space).
        if let Some(handle) = self.take_pending_focus() {
            window.focus(&handle, cx);
        } else if window.focused(cx).is_none() {
            let handle = self.default_focus();
            window.focus(&handle, cx);
        }

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
            // The root `"App"` context: global keys bind here, so a deeper
            // `"Browser"` or `"TextInput"` context shadows them when focused.
            // `track_focus` makes the root itself focusable: clicking empty space
            // auto-focuses it (innermost focusable wins, so fields still focus on
            // their own click), keeping the `"App"` context in the dispatch path.
            .key_context("App")
            .track_focus(&self.root_focus)
            .font_family(FONT_UI)
            .bg(theme.bg_panel_2)
            .text_color(theme.text)
            .text_sm()
            .on_action(cx.listener(|_, _: &FocusNext, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPrev, window, cx| window.focus_prev(cx)))
            .on_action(cx.listener(|_, _: &Quit, _, cx| cx.quit()))
            .on_action(cx.listener(|this, _: &NewConnection, _, cx| {
                this.open_editor_create(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.toggle_sidebar();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.open_settings();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ShowShortcuts, _, cx| {
                this.toggle_shortcuts();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusFilter, window, cx| {
                if this.view == View::Browse && !this.has_overlay() {
                    let handle = this.filter.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                if this.view == View::Browse {
                    this.refresh(cx);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| {
                if this.view == View::Browse && !this.has_overlay() {
                    this.disconnect();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                if this.dismiss_topmost_overlay(cx) {
                    cx.notify();
                }
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
            .when(self.shortcuts_open, |this| {
                let modal = shortcuts_modal(self, cx);
                this.child(modal)
            })
            // The connecting indicator sits under the prompts (mutually exclusive).
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
            .when(!self.pending_collisions.is_empty(), |this| {
                this.child(collision_modal(self, cx))
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

/// The password prompt shown before a connection is attempted.
fn password_modal(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();
    let prompt = state.password_prompt.as_ref().expect("password prompt set");
    let title = if prompt.is_passphrase {
        format!("Unlock key for {}", prompt.profile_name)
    } else {
        format!("Connect to {}", prompt.profile_name)
    };
    let save_label = if prompt.is_passphrase {
        "Save passphrase to keychain"
    } else {
        "Save to keychain"
    };

    Modal::new("password")
        .title(title)
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
                                .child(save_label),
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
/// mismatch never reaches here - it is rejected outright and surfaced as a toast.
fn host_key_modal(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();
    let prompt = state.host_key_prompt.as_ref().expect("host-key prompt set");
    let (title, saved_to) = if matches!(prompt.kind, nyx_core::ServerTrustKind::Certificate) {
        (
            "Verify certificate",
            "Trust this certificate and continue? It will be saved to known_certs.",
        )
    } else {
        (
            "Verify host key",
            "Trust this key and continue? It will be saved to known_hosts.",
        )
    };

    Modal::new("host-key")
        .title(title)
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
                .child(div().text_xs().text_color(theme.text_faint).child(saved_to)),
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
                        .focus_handle(state.modal_primary_focus.clone())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.trust_host_key();
                            cx.notify();
                        })),
                ),
        )
}

/// The file-collision prompt: the destination already exists. Overwrite / Skip /
/// Cancel, with an "Apply to all" toggle when more than one transfer is parked.
/// Dismissing (Esc / backdrop) is a Skip - never a silent overwrite.
fn collision_modal(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    use crate::state::models::fmt_size;
    use nyx_core::TransferDirection;

    let theme = cx.theme().clone();
    let info = state
        .pending_collisions
        .first()
        .expect("collision prompt set");
    let pending = state.pending_collisions.len();
    let apply_all = state.collision_apply_all;

    let side = match info.direction {
        TransferDirection::Upload => "remote",
        TransferDirection::Download => "local",
    };
    let noun = if info.is_dir { "folder" } else { "file" };
    let size_label = info
        .existing_size
        .map(|n| format!("Existing {side} file · {}", fmt_size(n)))
        .unwrap_or_else(|| format!("Existing {side} {noun}"));
    let title = if info.is_dir {
        "Folder already exists"
    } else {
        "File already exists"
    };

    Modal::new("collision")
        .title(title)
        .width(px(460.))
        .on_close({
            // Dismissing the prompt skips this transfer (never overwrites).
            let view = cx.entity();
            move |_window, cx| {
                view.update(cx, |this, cx| {
                    this.resolve_collision(CollisionChoice::Skip, cx);
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
                    "“{}” already exists at the destination.",
                    info.name
                )))
                .child(
                    div()
                        .p_3()
                        .rounded(theme.radius)
                        .bg(theme.bg_input)
                        .border_1()
                        .border_color(theme.border)
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .font_family(FONT_MONO)
                                .text_xs()
                                .text_color(theme.text)
                                .truncate()
                                .child(info.path.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.text_faint)
                                .child(size_label),
                        ),
                )
                .when(pending > 1, |this| {
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.text_muted)
                                    .child(format!("Apply to all {pending} conflicts")),
                            )
                            .child(
                                Toggle::new("collision-all", apply_all).on_change(cx.listener(
                                    |this, on, _window, cx| {
                                        this.set_collision_apply_all(*on);
                                        cx.notify();
                                    },
                                )),
                            ),
                    )
                }),
        )
        .footer(
            div()
                .flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("collision-cancel", "Cancel")
                        .variant(ButtonVariant::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.resolve_collision(CollisionChoice::Cancel, cx);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("collision-skip", "Skip")
                        .variant(ButtonVariant::Secondary)
                        .focus_handle(state.modal_primary_focus.clone())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.resolve_collision(CollisionChoice::Skip, cx);
                            cx.notify();
                        })),
                )
                .child(
                    // A folder merges into the existing tree (overwriting clashing
                    // files); a file is overwritten outright.
                    Button::new(
                        "collision-overwrite",
                        if info.is_dir { "Merge" } else { "Overwrite" },
                    )
                    .variant(ButtonVariant::Danger)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.resolve_collision(CollisionChoice::Overwrite, cx);
                        cx.notify();
                    })),
                ),
        )
}

/// The "remove connection?" confirmation (deletes the profile + its keychain entry).
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
            "Remove “{}”? This deletes the saved profile and its keychain password - this can't be undone.",
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
                        .focus_handle(state.modal_primary_focus.clone())
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

/// The browser file-row right-click menu. Download and Delete act on the whole
/// selection; Rename and Copy path on the clicked row.
fn file_context_menu(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let menu = state.file_menu.as_ref().expect("file menu set");
    let position = menu.position;

    let surface = ContextMenu::new("file-ctx")
        .item(
            ContextMenuItem::new("file-download", "Download").on_click(cx.listener(
                |this, _, _, cx| {
                    this.download_selection(cx);
                    cx.notify();
                },
            )),
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

/// The reusable single-field input modal (New folder / Rename).
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

/// The file-delete confirmation (one or many entries).
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
                        .focus_handle(state.modal_primary_focus.clone())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_file_delete(cx);
                            cx.notify();
                        })),
                ),
        )
}

/// A lightweight "connecting…" overlay.
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
                .child(crate::icon::spinner(
                    "connecting-spinner",
                    26.,
                    theme.accent,
                ))
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
    let auto_reconnect = state.auto_reconnect;
    let theme_ix = match cx.theme().name {
        "One Dark" => 0,
        "GitHub Dark" => 1,
        _ => 2,
    };
    let view = cx.entity();

    Modal::new("tweaks")
        .title("Tweaks")
        .width(px(420.))
        .on_close({
            let view = view.clone();
            move |_window, cx| {
                view.update(cx, |this, cx| {
                    this.tweaks_open = false;
                    this.theme_select_open = false;
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
                    Select::new("tw-theme")
                        .option("One Dark")
                        .option("GitHub Dark")
                        .option("Ayu Dark")
                        .selected(theme_ix)
                        .open(state.theme_select_open)
                        .on_toggle({
                            let view = view.clone();
                            move |_window, cx| {
                                view.update(cx, |this, cx| {
                                    this.theme_select_open = !this.theme_select_open;
                                    cx.notify();
                                });
                            }
                        })
                        .on_select({
                            let view = view.clone();
                            move |ix, _window, cx| {
                                let next = match ix {
                                    0 => Theme::one_dark(),
                                    1 => Theme::github_dark(),
                                    _ => Theme::ayu_dark(),
                                };
                                cx.set_global(next);
                                view.update(cx, |this, cx| {
                                    this.theme_select_open = false;
                                    this.save_settings(cx);
                                    cx.notify();
                                });
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
                                    this.save_settings(cx);
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
                                    this.save_settings(cx);
                                    cx.notify();
                                });
                            }
                        })),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.text_muted)
                                .child("Auto-reconnect"),
                        )
                        .child(Toggle::new("tw-auto-reconnect", auto_reconnect).on_change({
                            let view = view.clone();
                            move |on, _window, cx| {
                                let on = *on;
                                view.update(cx, |this, cx| {
                                    this.auto_reconnect = on;
                                    this.save_settings(cx);
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
                    .focus_handle(state.modal_primary_focus.clone())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tweaks_open = false;
                        this.theme_select_open = false;
                        cx.notify();
                    })),
            ),
        )
}

/// The keyboard-shortcuts cheat-sheet, rendered from the keymap table so it can
/// never drift from the actual bindings. Groups are spread across two balanced
/// columns to keep the dialog short.
fn shortcuts_modal(_state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();

    let mut col_a = div().flex().flex_col().gap_4().flex_1().min_w_0();
    let mut col_b = div().flex().flex_col().gap_4().flex_1().min_w_0();
    let (mut rows_a, mut rows_b) = (0usize, 0usize);

    // Greedily place each group in the currently-shorter column (groups are kept
    // whole, never split across columns).
    for (title, rows) in crate::keymap::cheat_sheet() {
        let n = rows.len();
        let section = shortcuts_section(title, rows, &theme);
        if rows_a <= rows_b {
            col_a = col_a.child(section);
            rows_a += n;
        } else {
            col_b = col_b.child(section);
            rows_b += n;
        }
    }

    Modal::new("shortcuts")
        .title("Keyboard shortcuts")
        .width(px(620.))
        .on_close(move |_window, cx| {
            view.update(cx, |this, cx| {
                this.shortcuts_open = false;
                cx.notify();
            });
        })
        .child(div().flex().gap_8().child(col_a).child(col_b))
}

/// One titled group of shortcut rows for the cheat-sheet.
fn shortcuts_section(
    title: &'static str,
    rows: Vec<(String, &'static str)>,
    theme: &Theme,
) -> impl IntoElement {
    let mut section = div().flex().flex_col().gap_1p5().child(
        div()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.text_faint)
            .child(title),
    );
    for (keys, label) in rows {
        section = section.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(div().text_sm().text_color(theme.text_muted).child(label))
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_xs()
                        .text_color(theme.text)
                        .px_1p5()
                        .py_0p5()
                        .rounded(theme.radius_sm)
                        .bg(theme.bg_input)
                        .border_1()
                        .border_color(theme.border)
                        .child(keys),
                ),
        );
    }
    section
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
