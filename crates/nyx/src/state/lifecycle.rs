//! Construction, settings persistence, and connection-list loading.

use super::*;

impl AppState {
    /// Build the initial state: welcome screen, connections loaded, nothing open.
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Not in the Tab ring - reachable via cmd-f, and keeping it out lets a
        // modal trap focus among its own fields/buttons.
        let filter = cx.new(|cx| {
            TextInput::new(cx)
                .with_placeholder("Filter")
                .tab_stop(false)
        });
        cx.observe(&filter, |this, _, cx| {
            this.refilter(cx);
            cx.notify();
        })
        .detach();
        // Esc/Enter in the filter hand focus back to the file table (it's out of
        // the Tab ring, so this is the only keyboard way out). The filter text is
        // left intact - Esc exits the field, it doesn't clear the filter.
        cx.subscribe(&filter, |this, _input, _event: &TextInputEvent, cx| {
            this.arm_focus(this.browser_focus.clone());
            cx.notify();
        })
        .detach();
        let browser_focus = cx.focus_handle();
        let root_focus = cx.focus_handle();
        let modal_primary_focus = cx.focus_handle().tab_stop(true);

        // Spawn the backend thread and drain its events into this entity. The
        // drain runs on the GPUI foreground executor: `next().await` yields, so it
        // never blocks the UI. This is the single Tokio↔GPUI bridge.
        let (service, mut events) = nyx_service::spawn();
        cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this
                    .update(cx, |state, cx| state.apply_event(event, cx))
                    .is_err()
                {
                    break; // entity gone → app is closing
                }
            }
        })
        .detach();

        // Missing file → empty list (first run); malformed → surfaced as a toast
        // once `Ready`, and the store is not overwritten so the user can fix it.
        let store = FileProfileStore::open_default()
            .unwrap_or_else(|_| FileProfileStore::with_path("profiles.toml"));
        let (connections, startup_error) = match store.list() {
            Ok(profiles) => (
                profiles
                    .into_iter()
                    .map(ConnectionVm::from_profile)
                    .collect(),
                None,
            ),
            Err(err) => (Vec::new(), Some(SharedString::from(err.to_string()))),
        };

        // A missing/malformed settings file is silently the default.
        let settings_store = FileSettingsStore::open_default()
            .unwrap_or_else(|_| FileSettingsStore::with_path("settings.toml"));
        let settings = settings_store.load();
        let theme_registry = ThemeRegistry::load();
        cx.set_global(theme_registry.by_name(&settings.theme));
        let density = Density::ALL[(settings.density as usize).min(Density::ALL.len() - 1)];
        let show_perms = settings.show_perms;
        let auto_reconnect = settings.auto_reconnect;

        let mut row_focus = HashMap::new();
        row_focus.insert("new".to_string(), cx.focus_handle().tab_stop(true));
        for conn in &connections {
            let id = &conn.profile.id;
            row_focus.insert(format!("card:{id}"), cx.focus_handle().tab_stop(true));
            row_focus.insert(format!("recent:{id}"), cx.focus_handle().tab_stop(true));
        }

        Self {
            view: View::Welcome,
            connections,
            active_id: None,
            online_id: None,
            cwd: RemotePath::root(),
            history: vec![RemotePath::root()],
            history_ix: 0,
            listing: Rc::new(Vec::new()),
            view_order: Rc::new(Vec::new()),
            filter,
            filter_query: Filter::default(),
            search: None,
            search_seq: 0,
            pending_select: None,
            sort: (SortKey::Name, true),
            selected: HashSet::new(),
            select_anchor: None,
            dock_open: true,
            dock_tab: DockTab::All,
            transfers: Vec::new(),
            // Hidden on the welcome screen (its cards are the connection
            // manager); shown once a connection opens. See `enter_browser`.
            sidebar_open: false,
            recent_collapsed: false,
            browser_focus,
            root_focus,
            pending_focus: None,
            modal_primary_focus,
            row_focus,
            tweaks_open: false,
            shortcuts_open: false,
            settings_tab: SettingsTab::default(),
            density,
            show_perms,
            toast: None,
            next_toast_id: 0,
            store,
            keyring: OsKeyring::new(),
            settings_store,
            theme_registry,
            startup_error,
            service,
            drag_downloads: DragDownloads::new(),
            drop_row_bounds: Rc::new(RefCell::new(Vec::new())),
            file_scroll: UniformListScrollHandle::new(),
            marquee: None,
            marquee_gen: 0,
            drag_return_folder: None,
            connecting_id: None,
            used_stored_password: None,
            password_prompt: None,
            host_key_prompt: None,
            pending_collisions: Vec::new(),
            collision_apply_all: false,
            editor: None,
            row_menu: None,
            delete_confirm: None,
            file_menu: None,
            input_prompt: None,
            file_delete: None,
            listing_loading: false,
            connection_lost: None,
            reconnect_attempt: None,
            reconnect_failed: false,
            auto_reconnect,
        }
    }

    /// Persist the current UI preferences to disk. Best-effort: a write failure
    /// is logged, not surfaced.
    pub fn save_settings(&self, cx: &App) {
        let settings = Settings {
            theme: cx.theme().name.to_string(),
            density: self.density.index() as u8,
            show_perms: self.show_perms,
            auto_reconnect: self.auto_reconnect,
        };
        if let Err(err) = self.settings_store.save(&settings) {
            tracing::warn!("failed to persist settings: {err}");
        }
    }

    /// All connections (the "Saved" group).
    pub fn connections_all(&self) -> Vec<&ConnectionVm> {
        self.connections.iter().collect()
    }

    /// The connection currently open in the browser, if any.
    pub fn active_conn(&self) -> Option<&ConnectionVm> {
        let id = self.active_id.as_deref()?;
        self.connections.iter().find(|c| c.profile.id == id)
    }

    /// Reload `connections` from the on-disk store (after a save/delete/stamp).
    pub(super) fn reload_connections(&mut self, cx: &mut Context<Self>) {
        match self.store.list() {
            Ok(profiles) => {
                self.connections = profiles
                    .into_iter()
                    .map(ConnectionVm::from_profile)
                    .collect();
                self.sync_row_focus(cx);
            }
            Err(err) => self.push_toast(err.to_string(), ToastVariant::Error, cx),
        }
    }

    /// Connections that count as "Recent", newest first.
    pub fn recent_connections(&self) -> Vec<&ConnectionVm> {
        let mut recents: Vec<&ConnectionVm> =
            self.connections.iter().filter(|c| c.is_recent).collect();
        recents.sort_by_key(|c| std::cmp::Reverse(c.profile.last_connected));
        recents
    }
}
