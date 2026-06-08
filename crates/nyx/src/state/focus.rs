//! Focus arming and per-row focus-handle management.

use super::*;

impl AppState {
    /// Ensure every connection row (and the New button) has a stable Tab-stop
    /// focus handle. Idempotent; called whenever the connection list changes.
    pub(super) fn sync_row_focus(&mut self, cx: &mut Context<Self>) {
        self.row_focus
            .entry("new".to_string())
            .or_insert_with(|| cx.focus_handle().tab_stop(true));
        let ids: Vec<String> = self
            .connections
            .iter()
            .map(|c| c.profile.id.clone())
            .collect();
        for id in ids {
            self.row_focus
                .entry(format!("card:{id}"))
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
            self.row_focus
                .entry(format!("recent:{id}"))
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
    }

    /// A stable focus handle for a welcome-list row, if one exists.
    pub fn row_focus(&self, key: &str) -> Option<FocusHandle> {
        self.row_focus.get(key).cloned()
    }

    /// Take the focus target queued for this render (modal autofocus, etc.).
    pub fn take_pending_focus(&mut self) -> Option<FocusHandle> {
        self.pending_focus.take()
    }

    /// The element that should hold focus when nothing else does: the file table
    /// while browsing (so arrow keys work), otherwise the root.
    pub fn default_focus(&self) -> FocusHandle {
        if self.view == View::Browse && !self.has_overlay() {
            self.browser_focus.clone()
        } else {
            self.root_focus.clone()
        }
    }

    /// Queue `handle` to receive focus on the next render.
    pub(super) fn arm_focus(&mut self, handle: FocusHandle) {
        self.pending_focus = Some(handle);
    }

    /// Queue the root for focus on the next render. Used for overlays with no
    /// primary button (menus, the cheat-sheet) so Esc still routes via `"App"`.
    pub(super) fn arm_root_focus(&mut self) {
        self.pending_focus = Some(self.root_focus.clone());
    }

    /// Queue the open modal's primary button for focus, so it shows the focus ring
    /// and Enter/Space activate it. Used for field-less confirmation modals.
    pub(super) fn arm_primary_focus(&mut self) {
        self.pending_focus = Some(self.modal_primary_focus.clone());
    }

    /// Queue a text field to receive focus on the next render (modal autofocus).
    pub(super) fn arm_input_focus(&mut self, input: &Entity<TextInput>, cx: &App) {
        self.pending_focus = Some(input.read(cx).focus_handle(cx));
    }

    /// Queue the editor's name field for focus on the next render.
    pub(super) fn arm_editor_focus(&mut self, cx: &App) {
        if let Some(name) = self.editor.as_ref().map(|e| e.name.clone()) {
            self.arm_input_focus(&name, cx);
        }
    }
}
