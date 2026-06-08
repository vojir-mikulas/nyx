//! Directory navigation: history, breadcrumbs, and listing reloads.

use super::*;

impl AppState {
    /// Enter the browser for a freshly-connected profile and list its starting
    /// directory: the profile's configured remote path if set, otherwise the
    /// server-resolved home (`home`) - so the user lands somewhere writable
    /// instead of the filesystem root.
    pub(super) fn enter_browser(
        &mut self,
        profile_id: String,
        home: RemotePath,
        cx: &mut Context<Self>,
    ) {
        let configured = self
            .connections
            .iter()
            .find(|c| c.profile.id == profile_id)
            .and_then(|c| c.profile.remote_path.as_deref())
            .map(str::trim)
            .filter(|p| !p.is_empty());
        let root = match configured {
            Some(path) => RemotePath::new(path),
            None => home,
        };

        self.active_id = Some(profile_id.clone());
        self.online_id = Some(profile_id.clone());
        self.connecting_id = None;
        self.view = View::Browse;
        self.sidebar_open = true;
        self.cwd = root.clone();
        self.history = vec![root];
        self.history_ix = 0;
        self.selected.clear();
        self.filter
            .update(cx, |input, cx| input.set_content("", cx));
        self.dock_open = true;
        self.transfers = Vec::new();
        self.pending_collisions.clear();
        self.collision_apply_all = false;
        // Focus the file table so keyboard navigation works the moment we land.
        self.arm_focus(self.browser_focus.clone());
        self.reload_listing(cx);
    }

    /// Replace the current listing and refresh the cached visible order.
    pub(super) fn set_listing(&mut self, listing: Vec<EntryRow>) {
        self.listing = Rc::new(listing);
        self.rebuild_view_order();
    }

    /// Request a listing for the current `cwd` from the backend, blanking the
    /// table first. The result arrives asynchronously as an [`Event::DirListing`].
    pub(super) fn reload_listing(&mut self, cx: &mut Context<Self>) {
        self.request_listing(true, cx);
    }

    /// Ask the backend for `cwd`'s listing. `clear` blanks the table first —
    /// right when changing directories, where the old rows are stale. An in-place
    /// refresh passes `false`: the current rows stay visible and a subtle toolbar
    /// spinner signals the reload, so the view doesn't flash empty.
    fn request_listing(&mut self, clear: bool, cx: &mut Context<Self>) {
        if clear {
            self.set_listing(Vec::new());
        }
        self.listing_loading = true;
        if !self.service.send(Command::ListDir {
            path: self.cwd.clone(),
        }) {
            self.listing_loading = false;
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Navigate to a path, optionally pushing onto the history stack.
    pub(super) fn go_to_path(
        &mut self,
        path: RemotePath,
        push_history: bool,
        cx: &mut Context<Self>,
    ) {
        self.cwd = path.clone();
        self.selected.clear();
        self.pending_select = None;
        self.filter
            .update(cx, |input, cx| input.set_content("", cx));
        if push_history {
            self.history.truncate(self.history_ix + 1);
            self.history.push(path);
            self.history_ix = self.history.len() - 1;
        }
        self.reload_listing(cx);
    }

    /// Open a child directory by name.
    pub fn open_dir(&mut self, name: &SharedString, cx: &mut Context<Self>) {
        let path = self.cwd.join(name);
        self.go_to_path(path, true, cx);
    }

    /// Jump to the `n`-th breadcrumb (0 = root): rebuild the prefix from the
    /// first `n` components of the current path.
    pub fn nav_crumb(&mut self, n: usize, cx: &mut Context<Self>) {
        let mut path = RemotePath::root();
        for seg in self.cwd.components().take(n) {
            path = path.join(seg);
        }
        self.go_to_path(path, true, cx);
    }

    /// Go up one directory level.
    pub fn go_up(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self.cwd.parent() {
            self.go_to_path(parent, true, cx);
        }
    }

    pub fn can_back(&self) -> bool {
        self.history_ix > 0
    }

    pub fn can_forward(&self) -> bool {
        self.history_ix + 1 < self.history.len()
    }

    /// Step back in history.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        if !self.can_back() {
            return;
        }
        self.history_ix -= 1;
        let path = self.history[self.history_ix].clone();
        self.go_to_path(path, false, cx);
    }

    /// Step forward in history.
    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if !self.can_forward() {
            return;
        }
        self.history_ix += 1;
        let path = self.history[self.history_ix].clone();
        self.go_to_path(path, false, cx);
    }

    /// Refresh the current listing in place, keeping the existing rows visible.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.request_listing(false, cx);
    }
}
