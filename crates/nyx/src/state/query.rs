//! Sort, filter, and recursive tree search.

use super::*;

impl AppState {
    /// Cycle the sort for a clicked column header.
    pub fn toggle_sort(&mut self, column: usize) {
        let Some(key) = SortKey::from_column(column) else {
            return;
        };
        self.sort = if self.sort.0 == key {
            (key, !self.sort.1)
        } else {
            (key, true)
        };
        self.rebuild_view_order();
    }

    /// The current filter text (lower-cased compare happens in the getter).
    pub fn filter_text(&self, cx: &App) -> String {
        self.filter.read(cx).content().to_string()
    }

    /// React to a filter-box change: a `/`-scoped, non-empty query (on a live
    /// connection) starts/refreshes a recursive tree search; anything else cancels
    /// any search and filters the current directory in place.
    pub(super) fn refilter(&mut self, cx: &App) {
        let text = self.filter.read(cx).content();
        self.filter_query = Filter::parse(text.as_ref());
        if self.filter_query.scope() == Scope::Tree
            && !self.filter_query.is_empty()
            && self.online_id.is_some()
        {
            self.start_tree_search(text);
        } else {
            self.end_tree_search();
            self.rebuild_view_order();
        }
    }

    /// The active tree search, if any (the browser renders its results in place
    /// of the file table).
    pub fn search(&self) -> Option<&SearchState> {
        self.search.as_ref()
    }

    /// Begin (or supersede) a recursive search of the current subtree. Each call
    /// mints a fresh token; the backend aborts the prior walk when the new
    /// `SearchTree` arrives, and stale-token batches are ignored on arrival.
    pub(super) fn start_tree_search(&mut self, query_text: SharedString) {
        self.search_seq += 1;
        let token = self.search_seq;
        let root = self.cwd.clone();
        self.search = Some(SearchState {
            token,
            query: query_text,
            hits: Rc::new(Vec::new()),
            done: false,
            truncated: false,
        });
        self.service.send(Command::SearchTree {
            root,
            query: self.filter_query.clone(),
            token,
        });
    }

    /// Leave search mode, telling the backend to drop any in-flight walk.
    pub(super) fn end_tree_search(&mut self) {
        if self.search.take().is_some() {
            self.service.send(Command::CancelSearch);
        }
    }

    /// Open a search hit: navigate to its parent directory and select it there.
    pub fn activate_search_hit(&mut self, path: &RemotePath, cx: &mut Context<Self>) {
        let Some(parent) = path.parent() else { return };
        let name = path.file_name().map(SharedString::from);
        // `go_to_path` clears the filter (→ leaves search) and reloads; arm the
        // selection so it lands once the new listing arrives.
        self.go_to_path(parent, true, cx);
        self.pending_select = name;
    }
}
