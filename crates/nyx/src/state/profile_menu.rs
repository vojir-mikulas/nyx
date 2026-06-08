//! Sidebar connection row context menu and profile deletion.

use super::*;

impl AppState {
    /// Open the sidebar row context menu (Edit / Remove) at a cursor position.
    pub fn open_row_menu(
        &mut self,
        profile_id: String,
        profile_name: SharedString,
        position: Point<Pixels>,
    ) {
        self.row_menu = Some(RowMenu {
            profile_id,
            profile_name,
            position,
        });
        self.arm_root_focus();
    }

    /// Dismiss the row context menu.
    pub fn close_row_menu(&mut self) {
        self.row_menu = None;
    }

    /// Open the "remove connection?" confirmation for a profile.
    pub fn open_delete_confirm(&mut self, profile_id: String, profile_name: SharedString) {
        self.row_menu = None;
        self.delete_confirm = Some(DeleteConfirm {
            profile_id,
            profile_name,
        });
        self.arm_primary_focus();
    }

    /// Dismiss the delete confirmation without deleting.
    pub fn cancel_delete(&mut self) {
        self.delete_confirm = None;
    }

    /// Delete the confirmed profile from the store and its keychain entry.
    pub fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.delete_confirm.take() else {
            return;
        };
        let id = confirm.profile_id;
        if let Err(err) = self.store.delete(&id) {
            self.push_toast(err.to_string(), ToastVariant::Error, cx);
            return;
        }
        self.keyring_clear_async(id.clone(), cx);
        if self.editor.as_ref().is_some_and(|e| e.id == id) {
            self.editor = None;
        }
        self.reload_connections(cx);
        self.push_toast(
            format!("Removed “{}”", confirm.profile_name),
            ToastVariant::Success,
            cx,
        );
    }
}
