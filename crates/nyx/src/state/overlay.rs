//! Top-level overlays, the sidebar, and toasts.

use super::*;

impl AppState {
    /// Toggle the sidebar **Recent** group's collapsed state.
    pub fn toggle_recent_collapsed(&mut self) {
        self.recent_collapsed = !self.recent_collapsed;
    }

    /// Whether any overlay - a modal, prompt, context menu, the cheat-sheet, or
    /// the connecting spinner - is currently on screen. The browser drops its key
    /// context while this holds, so global Enter/Esc route to the overlay instead
    /// of the file table beneath it.
    pub fn has_overlay(&self) -> bool {
        self.editor.is_some()
            || self.password_prompt.is_some()
            || self.host_key_prompt.is_some()
            || !self.pending_collisions.is_empty()
            || self.delete_confirm.is_some()
            || self.file_delete.is_some()
            || self.input_prompt.is_some()
            || self.tweaks_open
            || self.shortcuts_open
            || self.row_menu.is_some()
            || self.file_menu.is_some()
            || self.connecting_id.is_some()
    }

    /// Toggle the sidebar's visibility (the `cmd-b` global).
    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
    }

    /// Open the settings panel (`cmd-,`).
    pub fn open_settings(&mut self) {
        self.tweaks_open = true;
        self.arm_primary_focus();
    }

    /// Switch the settings panel's selected category.
    pub fn set_settings_tab(&mut self, tab: SettingsTab) {
        self.settings_tab = tab;
    }

    /// Make `name` the active theme and persist the choice.
    pub fn select_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        cx.set_global(self.theme_registry.by_name(name));
        self.save_settings(cx);
        cx.notify();
    }

    /// Delete a custom theme's file, then reload the registry. If the removed
    /// theme was active, fall back to the default. Built-ins can't be removed.
    pub fn remove_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        match self.theme_registry.remove(name) {
            Ok(()) => {
                let was_active = cx.theme().name.as_str() == name;
                self.theme_registry = ThemeRegistry::load();
                if was_active {
                    cx.set_global(self.theme_registry.by_name("One Dark"));
                    self.save_settings(cx);
                }
                self.push_toast(format!("Removed theme “{name}”"), ToastVariant::Success, cx);
                cx.notify();
            }
            Err(err) => self.push_toast(err, ToastVariant::Error, cx),
        }
    }

    /// Pick a theme `.toml` from disk, install it into the config `themes/` dir,
    /// then make it the active theme. Validation/copy lives in
    /// [`crate::theme_load::install_theme`]; a bad file becomes an error toast and
    /// nothing is written.
    pub fn import_theme(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Add theme".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                match crate::theme_load::install_theme(&path) {
                    Ok(name) => {
                        this.theme_registry = ThemeRegistry::load();
                        cx.set_global(this.theme_registry.by_name(&name));
                        this.save_settings(cx);
                        this.push_toast(format!("Added theme “{name}”"), ToastVariant::Success, cx);
                        cx.notify();
                    }
                    Err(err) => this.push_toast(err, ToastVariant::Error, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Toggle the keyboard-shortcuts cheat-sheet (`cmd-/`).
    pub fn toggle_shortcuts(&mut self) {
        self.shortcuts_open = !self.shortcuts_open;
        if self.shortcuts_open {
            self.arm_root_focus();
        }
    }

    /// Esc handler: dismiss the topmost overlay - menus first, then the cheat
    /// sheet, then prompts/modals in z-order. Returns whether anything closed.
    /// Each dismissal is the modal's own cancel (e.g. a collision Skip), never a
    /// destructive default.
    pub fn dismiss_topmost_overlay(&mut self, cx: &mut Context<Self>) -> bool {
        if self.row_menu.is_some() {
            self.row_menu = None;
        } else if self.file_menu.is_some() {
            self.file_menu = None;
        } else if self.shortcuts_open {
            self.shortcuts_open = false;
        } else if self.editor.is_some() {
            self.close_editor();
        } else if self.password_prompt.is_some() {
            self.cancel_password();
        } else if self.host_key_prompt.is_some() {
            self.reject_host_key();
        } else if !self.pending_collisions.is_empty() {
            self.resolve_collision(CollisionChoice::Skip, cx);
        } else if self.delete_confirm.is_some() {
            self.cancel_delete();
        } else if self.file_delete.is_some() {
            self.cancel_file_delete();
        } else if self.input_prompt.is_some() {
            self.cancel_input();
        } else if self.tweaks_open {
            self.tweaks_open = false;
        } else {
            return false;
        }
        true
    }

    /// Show a toast that auto-dismisses after a short delay.
    pub fn push_toast(
        &mut self,
        message: impl Into<SharedString>,
        variant: ToastVariant,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toast = Some(ToastMsg {
            message: message.into(),
            variant,
            id,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(2600))
                .await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|t| t.id == id) {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}
