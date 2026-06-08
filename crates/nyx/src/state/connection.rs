//! Opening connections: auth prompts, the connect handshake, host-key trust, disconnect and reconnect.

use super::*;

impl AppState {
    /// Begin opening a connection: look the password up in the keychain
    /// off-thread, then either connect straight through (hit) or prompt (miss).
    /// `connecting_id` is set up-front so the UI shows progress while the
    /// (potentially dialog-popping) keychain lookup runs off the GPUI thread.
    pub fn open_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.connections.iter().find(|c| c.profile.id == id) else {
            return;
        };
        let profile = conn.profile.clone();
        self.connecting_id = Some(id.to_string());

        // Anonymous login carries no secret: skip the keychain entirely.
        if matches!(profile.auth, AuthMethod::Anonymous) {
            self.send_connect(profile, String::new(), false, cx);
            return;
        }

        let keyring = self.keyring;
        let account = secret_account(&profile);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { keyring.get_password(KEYCHAIN_SERVICE, &account) })
                .await;
            this.update(cx, |this, cx| {
                this.on_password_lookup(profile, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Apply the result of the keychain lookup started by [`open_connection`].
    pub(super) fn on_password_lookup(
        &mut self,
        profile: Profile,
        result: nyx_core::Result<Option<String>>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(secret)) => self.send_connect(profile, secret, true, cx),
            // No stored secret. For password auth, prompt. For key auth, try with
            // no passphrase (an unencrypted key needs none) — an encrypted key
            // comes back as `KeyLocked` and we prompt for the passphrase then.
            Ok(None) => match profile.auth {
                AuthMethod::Password => self.show_password_prompt(profile, cx),
                AuthMethod::Key { .. } | AuthMethod::Anonymous => {
                    self.send_connect(profile, String::new(), false, cx)
                }
            },
            Err(err) => {
                self.connecting_id = None;
                self.push_toast(format!("Keychain error: {err}"), ToastVariant::Error, cx);
            }
        }
    }

    /// Show the password prompt for `profile` (a keychain miss, or a stale-secret
    /// re-prompt).
    pub(super) fn show_password_prompt(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.show_secret_prompt(profile, false, cx);
    }

    /// Show the passphrase prompt for `profile` (a locked key, or a stale-secret
    /// re-prompt).
    pub(super) fn show_passphrase_prompt(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.show_secret_prompt(profile, true, cx);
    }

    /// Build and show the secret prompt — a password or, when `is_passphrase`, a
    /// key passphrase. The "Save to keychain" toggle defaults on.
    pub(super) fn show_secret_prompt(
        &mut self,
        profile: Profile,
        is_passphrase: bool,
        cx: &mut Context<Self>,
    ) {
        let host_label = format!("{}@{}:{}", profile.username, profile.host, profile.port);
        let placeholder = if is_passphrase {
            "Passphrase"
        } else {
            "Password"
        };
        let input = cx.new(|cx| TextInput::new(cx).with_placeholder(placeholder).obscured());
        self.wire_input(&input, cx);
        self.arm_input_focus(&input, cx);
        self.password_prompt = Some(PasswordPrompt {
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone().into(),
            host_label: host_label.into(),
            input,
            save_to_keychain: true,
            is_passphrase,
        });
    }

    /// Send a `Connect` command, tracking whether a *stored* secret was used.
    pub(super) fn send_connect(
        &mut self,
        profile: Profile,
        secret: String,
        from_keychain: bool,
        cx: &mut Context<Self>,
    ) {
        let id = profile.id.clone();
        self.connecting_id = Some(id.clone());
        self.used_stored_password = from_keychain.then_some(id);
        let sent = self.service.send(Command::Connect {
            profile,
            secret: Secret::new(secret),
            auto_reconnect: self.auto_reconnect,
        });
        if !sent {
            self.connecting_id = None;
            self.used_stored_password = None;
            self.push_toast("Backend unavailable", ToastVariant::Error, cx);
        }
    }

    /// Submit the password prompt: optionally save the secret to the keychain
    /// (off-thread, *before* connecting so it persists even if connect fails),
    /// then send `Connect`.
    pub fn confirm_password(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.password_prompt.take() else {
            return;
        };
        let secret = prompt.input.read(cx).content().to_string();
        let Some(conn) = self
            .connections
            .iter()
            .find(|c| c.profile.id == prompt.profile_id)
        else {
            return;
        };
        let profile = conn.profile.clone();
        let account = if prompt.is_passphrase {
            passphrase_account(&profile.id)
        } else {
            password_account(&profile.id)
        };

        if prompt.save_to_keychain && !secret.is_empty() {
            self.save_secret_then_connect(profile, account, secret, cx);
        } else {
            self.send_connect(profile, secret, false, cx);
        }
    }

    /// Save a secret to the keychain off-thread, then send `Connect` once the
    /// write completes (a failed save is a non-fatal toast — we still connect).
    pub(super) fn save_secret_then_connect(
        &mut self,
        profile: Profile,
        account: String,
        secret: String,
        cx: &mut Context<Self>,
    ) {
        self.connecting_id = Some(profile.id.clone());
        let keyring = self.keyring;
        let secret_for_save = secret.clone();
        cx.spawn(async move |this, cx| {
            let saved =
                cx.background_executor()
                    .spawn(async move {
                        keyring.set_password(KEYCHAIN_SERVICE, &account, &secret_for_save)
                    })
                    .await;
            this.update(cx, |this, cx| {
                if saved.is_err() {
                    this.push_toast("Couldn't save secret to keychain", ToastVariant::Error, cx);
                }
                // A freshly-typed secret isn't a "stored" one for the auth-retry
                // heuristic, even though we just saved it.
                this.send_connect(profile, secret, false, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Dismiss the password prompt without connecting.
    pub fn cancel_password(&mut self) {
        self.password_prompt = None;
        self.connecting_id = None;
        self.used_stored_password = None;
    }

    /// Toggle the password prompt's "Save to keychain" switch.
    pub fn set_password_save(&mut self, on: bool) {
        if let Some(prompt) = self.password_prompt.as_mut() {
            prompt.save_to_keychain = on;
        }
    }

    /// Subscribe to a modal field's [`TextInputEvent`]s so Enter submits and Esc
    /// dismisses the modal it belongs to. Dispatch is routed by *which* modal is
    /// open (mutually exclusive). The filter box is deliberately not wired.
    pub(super) fn wire_input(&self, input: &Entity<TextInput>, cx: &mut Context<Self>) {
        cx.subscribe(input, |this, _input, event, cx| {
            match event {
                TextInputEvent::Submit => this.submit_focused_modal(cx),
                TextInputEvent::Cancel => this.cancel_focused_modal(),
            }
            cx.notify();
        })
        .detach();
    }

    /// Route an Enter from a wired field to the open modal's primary action.
    pub(super) fn submit_focused_modal(&mut self, cx: &mut Context<Self>) {
        if self.password_prompt.is_some() {
            self.confirm_password(cx);
        } else if self.input_prompt.is_some() {
            self.submit_input(cx);
        } else if self.editor.is_some() {
            self.save_editor(cx);
        }
    }

    /// Route an Esc from a wired field to the open modal's dismiss action.
    pub(super) fn cancel_focused_modal(&mut self) {
        if self.password_prompt.is_some() {
            self.cancel_password();
        } else if self.input_prompt.is_some() {
            self.cancel_input();
        } else if self.editor.is_some() {
            self.close_editor();
        }
    }

    /// Trust the pending host key and continue connecting.
    pub fn trust_host_key(&mut self) {
        self.host_key_prompt = None;
        self.service.send(Command::HostKeyDecision { accept: true });
    }

    /// Reject the pending host key and abort the connection.
    pub fn reject_host_key(&mut self) {
        self.host_key_prompt = None;
        self.service
            .send(Command::HostKeyDecision { accept: false });
    }

    /// Close the active connection and return to the welcome screen.
    pub fn disconnect(&mut self) {
        self.service.send(Command::Disconnect);
        self.active_id = None;
        self.online_id = None;
        self.connecting_id = None;
        self.view = View::Welcome;
        self.transfers.clear();
        self.pending_collisions.clear();
        self.collision_apply_all = false;
        self.set_listing(Vec::new());
        self.selected.clear();
        self.listing_loading = false;
        self.connection_lost = None;
        self.reconnect_attempt = None;
        self.reconnect_failed = false;
    }

    /// Reconnect after a connection loss: re-issue the connect for the active
    /// profile (re-fetching the secret from the keyring as on first connect). The
    /// connecting overlay covers the banner; the banner stays set underneath so a
    /// *failed* reconnect leaves it in place to retry. Success
    /// ([`Event::Connected`]) clears it and re-enters the browser.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_id.clone() else {
            return;
        };
        self.reconnect_attempt = None;
        self.reconnect_failed = false;
        self.open_connection(&id, cx);
    }

    /// Stop the in-progress auto-reconnect backoff loop, leaving the session lost
    /// with a manual-reconnect affordance (the banner drops its Cancel).
    pub fn cancel_reconnect(&mut self) {
        self.service.send(Command::CancelReconnect);
        self.reconnect_attempt = None;
        self.reconnect_failed = false;
    }
}
