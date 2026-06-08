//! Browser file operations: context menu, new folder, rename, delete, activation, and copy-path.

use super::*;

impl AppState {
    /// Open the file-row context menu. Right-click on an unselected row replaces
    /// the selection with just it; right-click inside the selection keeps it.
    pub fn open_file_menu(&mut self, name: SharedString, position: Point<Pixels>) {
        if !self.selected.contains(&name) {
            self.selected.clear();
            self.selected.insert(name.clone());
        }
        self.file_menu = Some(FileMenu { name, position });
        self.arm_root_focus();
    }

    /// Dismiss the file-row context menu.
    pub fn close_file_menu(&mut self) {
        self.file_menu = None;
    }

    /// Open the **New folder** input modal (blank, "Create").
    pub fn start_new_folder(&mut self, cx: &mut Context<Self>) {
        self.close_file_menu();
        let input = cx.new(|cx| TextInput::new(cx).with_placeholder("Folder name"));
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        self.wire_input(&input, cx);
        self.arm_input_focus(&input, cx);
        self.input_prompt = Some(InputPrompt {
            title: "New folder".into(),
            label: "Name".into(),
            submit_label: "Create".into(),
            input,
            action: InputAction::NewFolder,
        });
    }

    /// Open the **Rename** input modal, prefilled with the clicked row's name.
    pub fn start_rename(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.file_menu.as_ref() else {
            return;
        };
        let name = menu.name.clone();
        self.close_file_menu();
        self.open_rename_prompt(name, cx);
    }

    /// Open the **Rename** modal for the current single-row selection — the
    /// keyboard (F2) entry point that has no context menu to read.
    pub fn rename_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.len() != 1 {
            return;
        }
        let Some(name) = self.selected.iter().next().cloned() else {
            return;
        };
        self.open_rename_prompt(name, cx);
    }

    /// Build and show the rename modal for `name` (shared by the menu + F2).
    pub(super) fn open_rename_prompt(&mut self, name: SharedString, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new(cx).with_content(name.clone()));
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        self.wire_input(&input, cx);
        self.arm_input_focus(&input, cx);
        self.input_prompt = Some(InputPrompt {
            title: "Rename".into(),
            label: "New name".into(),
            submit_label: "Rename".into(),
            input,
            action: InputAction::Rename { original: name },
        });
    }

    /// Activate the current selection (the browser's Enter key): a single
    /// selected directory is opened, a symlink is resolved (navigate or
    /// download), otherwise the selection is downloaded.
    pub fn activate_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.len() == 1 {
            if let Some(name) = self.selected.iter().next().cloned() {
                match self.entry_kind(&name) {
                    Some(EntryKind::Directory) => {
                        self.open_dir(&name, cx);
                        return;
                    }
                    Some(EntryKind::Symlink) => {
                        self.open_symlink(&name, cx);
                        return;
                    }
                    _ => {}
                }
            }
        }
        self.download_selection(cx);
    }

    /// Activate one row by name (double-click): open a directory, resolve a
    /// symlink, or do nothing for a plain file (Enter/menu drive file actions).
    pub fn activate_row(&mut self, name: &SharedString, cx: &mut Context<Self>) {
        match self.entry_kind(name) {
            Some(EntryKind::Directory) => self.open_dir(name, cx),
            Some(EntryKind::Symlink) => self.open_symlink(name, cx),
            _ => {}
        }
    }

    /// The kind of a listed entry by name, if present.
    pub(super) fn entry_kind(&self, name: &SharedString) -> Option<EntryKind> {
        self.listing
            .iter()
            .find(|row| row.entry.name.as_str() == name.as_ref())
            .map(|row| row.entry.kind)
    }

    /// Resolve a symlink on click: ask the backend to follow it. The reply
    /// ([`Event::SymlinkResolved`]) navigates into a directory target or
    /// downloads a file target — one round-trip, paid only on activation.
    pub fn open_symlink(&mut self, name: &SharedString, cx: &mut Context<Self>) {
        let path = self.cwd.join(name);
        if !self.service.send(Command::ResolveSymlink { path }) {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Dismiss the input modal without acting.
    pub fn cancel_input(&mut self) {
        self.input_prompt = None;
    }

    /// Validate and submit the input modal → `Mkdir` / `Rename`. Rejects an empty
    /// name or one containing `/`; an unchanged rename is a no-op.
    pub fn submit_input(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.input_prompt.as_ref() else {
            return;
        };
        let value = prompt.input.read(cx).content().trim().to_string();
        if value.is_empty() {
            self.push_toast("Name can't be empty", ToastVariant::Error, cx);
            return;
        }
        if value.contains('/') {
            self.push_toast("Name can't contain a slash", ToastVariant::Error, cx);
            return;
        }
        let action = prompt.action.clone();
        self.input_prompt = None;
        let command = match action {
            InputAction::NewFolder => Command::Mkdir {
                path: self.cwd.join(&value),
            },
            InputAction::Rename { original } => {
                if value == original.as_ref() {
                    return; // unchanged → nothing to do
                }
                Command::Rename {
                    from: self.cwd.join(&original),
                    to: self.cwd.join(&value),
                }
            }
        };
        if !self.service.send(command) {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Open the file-delete confirmation for the current selection.
    pub fn start_delete(&mut self, _cx: &mut Context<Self>) {
        self.close_file_menu();
        let entries: Vec<(SharedString, bool)> = self
            .selected
            .iter()
            .filter_map(|name| {
                self.listing
                    .iter()
                    .find(|row| row.entry.name.as_str() == name.as_ref())
                    .map(|row| (name.clone(), row.entry.is_dir()))
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        self.file_delete = Some(FileDeleteConfirm { entries });
        self.arm_primary_focus();
    }

    /// Dismiss the file-delete confirmation without deleting.
    pub fn cancel_file_delete(&mut self) {
        self.file_delete = None;
    }

    /// Issue one `Remove` per confirmed entry (file or recursive folder).
    pub fn confirm_file_delete(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.file_delete.take() else {
            return;
        };
        let mut ok = true;
        for (name, is_dir) in &confirm.entries {
            let path = self.cwd.join(name);
            if !self.service.send(Command::Remove {
                path,
                is_dir: *is_dir,
            }) {
                ok = false;
            }
        }
        if !ok {
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Copy the single-selected entry's absolute remote path to the clipboard
    /// (keyboard `cmd-c`, mirroring the row menu's Copy path).
    pub fn copy_selection_path(&mut self, cx: &mut Context<Self>) {
        if self.selected.len() != 1 {
            return;
        }
        let Some(name) = self.selected.iter().next().cloned() else {
            return;
        };
        let path = self.cwd.join(&name);
        cx.write_to_clipboard(ClipboardItem::new_string(path.as_str().to_string()));
        self.push_toast("Path copied", ToastVariant::Success, cx);
    }

    /// Copy the clicked entry's absolute remote path to the clipboard.
    pub fn copy_path(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.file_menu.take() else {
            return;
        };
        let path = self.cwd.join(&menu.name);
        cx.write_to_clipboard(ClipboardItem::new_string(path.as_str().to_string()));
        self.push_toast("Path copied", ToastVariant::Success, cx);
    }
}
