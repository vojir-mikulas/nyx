//! The connection editor modal: field wiring, validation, save, and the test-connection probe.

use super::*;

impl AppState {
    /// Open the editor in **Create** mode (a fresh id, blank form).
    pub fn open_editor_create(&mut self, cx: &mut Context<Self>) {
        self.row_menu = None;
        self.editor = Some(ConnectionEditor {
            id: Profile::new_id(),
            is_new: true,
            name: cx.new(|cx| TextInput::new(cx).with_placeholder("My server")),
            host: cx.new(|cx| TextInput::new(cx).with_placeholder("example.com")),
            port: cx.new(|cx| TextInput::new(cx).with_placeholder("22").with_content("22")),
            username: cx.new(|cx| TextInput::new(cx).with_placeholder("user")),
            remote_path: cx.new(|cx| TextInput::new(cx).with_placeholder("/var/www  (optional)")),
            password: cx.new(|cx| TextInput::new(cx).with_placeholder("Password").obscured()),
            auth_is_key: false,
            auth_is_anonymous: false,
            key_path: cx.new(|cx| TextInput::new(cx).with_placeholder("~/.ssh/id_ed25519")),
            passphrase: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("Passphrase  (optional)")
                    .obscured()
            }),
            protocol: Protocol::Sftp,
            ftps_mode: FtpsMode::default(),
            color: AccentKind::Purple,
            test_status: None,
            testing: false,
        });
        self.wire_editor_inputs(cx);
        self.arm_editor_focus(cx);
    }

    /// Open the editor in **Edit** mode, prefilled from an existing profile. The
    /// password field stays blank (we never read the secret back out of the
    /// keychain to display it; blank on save means "keep the stored secret").
    pub fn open_editor_edit(&mut self, id: &str, cx: &mut Context<Self>) {
        self.row_menu = None;
        let Some(conn) = self.connections.iter().find(|c| c.profile.id == id) else {
            return;
        };
        let p = conn.profile.clone();
        let (auth_is_key, auth_is_anonymous, key_path_str) = match &p.auth {
            AuthMethod::Password => (false, false, String::new()),
            AuthMethod::Key { path } => (true, false, path.display().to_string()),
            AuthMethod::Anonymous => (false, true, String::new()),
        };
        self.editor = Some(ConnectionEditor {
            id: p.id.clone(),
            is_new: false,
            name: cx.new(|cx| TextInput::new(cx).with_content(p.name.clone())),
            host: cx.new(|cx| TextInput::new(cx).with_content(p.host.clone())),
            port: cx.new(|cx| TextInput::new(cx).with_content(p.port.to_string())),
            username: cx.new(|cx| TextInput::new(cx).with_content(p.username.clone())),
            remote_path: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("/var/www  (optional)")
                    .with_content(p.remote_path.clone().unwrap_or_default())
            }),
            password: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("Leave blank to keep current")
                    .obscured()
            }),
            auth_is_key,
            auth_is_anonymous,
            key_path: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("~/.ssh/id_ed25519")
                    .with_content(key_path_str)
            }),
            passphrase: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("Leave blank to keep current")
                    .obscured()
            }),
            protocol: p.protocol,
            ftps_mode: p.ftps_mode,
            color: AccentKind::from_profile_color(p.color),
            test_status: None,
            testing: false,
        });
        self.wire_editor_inputs(cx);
        self.arm_editor_focus(cx);
    }

    /// Wire every editor field's submit/cancel events (Enter saves, Esc closes).
    pub(super) fn wire_editor_inputs(&self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let inputs = [
            editor.name.clone(),
            editor.host.clone(),
            editor.port.clone(),
            editor.username.clone(),
            editor.remote_path.clone(),
            editor.password.clone(),
            editor.key_path.clone(),
            editor.passphrase.clone(),
        ];
        for input in &inputs {
            self.wire_input(input, cx);
        }
    }

    /// Switch the editor's auth method. Index 0 is always Password; index 1 is
    /// Key under SFTP and Anonymous under FTP/FTPS (the two are never both shown).
    pub fn set_editor_auth(&mut self, ix: usize) {
        if let Some(editor) = self.editor.as_mut() {
            let second_is_key = editor.protocol == Protocol::Sftp;
            editor.auth_is_key = ix == 1 && second_is_key;
            editor.auth_is_anonymous = ix == 1 && !second_is_key;
        }
    }

    /// Open a native file picker for the private key and write the chosen path
    /// into the editor's key-path field.
    pub fn pick_key_file(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose key".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                if let Some(editor) = this.editor.as_ref() {
                    editor.key_path.update(cx, |input, cx| {
                        input.set_content(path.display().to_string(), cx);
                    });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Close the editor without saving.
    pub fn close_editor(&mut self) {
        self.editor = None;
    }

    /// Change the editor's protocol, applying the new default port when the port
    /// field is blank or still holds the previous protocol's default.
    pub fn set_editor_protocol(&mut self, ix: usize, cx: &mut Context<Self>) {
        let new = match ix {
            1 => Protocol::Ftp,
            2 => Protocol::Ftps,
            _ => Protocol::Sftp,
        };
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let old = editor.protocol;
        editor.protocol = new;
        // Key auth is SFTP-only and anonymous is FTP/FTPS-only; a protocol switch
        // clears whichever no longer applies, leaving password auth as the default.
        if new == Protocol::Sftp {
            editor.auth_is_anonymous = false;
        } else {
            editor.auth_is_key = false;
        }
        let port_input = editor.port.clone();
        let port_text = port_input.read(cx).content().to_string();
        let port_trim = port_text.trim();
        let holds_old_default = port_trim.parse::<u16>().ok() == Some(old.default_port());
        if port_trim.is_empty() || holds_old_default {
            port_input.update(cx, |input, cx| {
                input.set_content(new.default_port().to_string(), cx)
            });
        }
    }

    /// Change the editor's FTPS TLS mode (0 = explicit, 1 = implicit). When the
    /// port still holds the protocol default, switch it to the conventional
    /// implicit/explicit port (990 / 21).
    pub fn set_editor_ftps_mode(&mut self, ix: usize, cx: &mut Context<Self>) {
        let new = if ix == 1 {
            FtpsMode::Implicit
        } else {
            FtpsMode::Explicit
        };
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        editor.ftps_mode = new;
        // Nudge the port to the conventional one when it still holds a default.
        let port_input = editor.port.clone();
        let port_text = port_input.read(cx).content().to_string();
        let port_trim = port_text.trim();
        if port_trim.is_empty() || port_trim == "21" || port_trim == "990" {
            let port = if new == FtpsMode::Implicit {
                "990"
            } else {
                "21"
            };
            port_input.update(cx, |input, cx| input.set_content(port, cx));
        }
    }

    /// Change the editor's accent color by picker index.
    pub fn set_editor_color(&mut self, ix: usize) {
        if let Some(editor) = self.editor.as_mut() {
            editor.color = AccentKind::ALL.get(ix).copied().unwrap_or(AccentKind::Blue);
        }
    }

    /// Validate and save the editor's profile (and its password, if entered).
    pub fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let name = editor.name.read(cx).content().trim().to_string();
        let host = editor.host.read(cx).content().trim().to_string();
        let port_text = editor.port.read(cx).content().trim().to_string();
        let username = editor.username.read(cx).content().trim().to_string();
        let remote_path = editor.remote_path.read(cx).content().trim().to_string();
        let password = editor.password.read(cx).content().to_string();
        let auth_is_key = editor.auth_is_key;
        let auth_is_anonymous = editor.auth_is_anonymous;
        let key_path = editor.key_path.read(cx).content().trim().to_string();
        let passphrase = editor.passphrase.read(cx).content().to_string();
        let protocol = editor.protocol;
        let ftps_mode = editor.ftps_mode;
        let color = editor.color;
        let id = editor.id.clone();
        let is_new = editor.is_new;

        // Anonymous login ignores the username; everything else requires one.
        if name.is_empty() || host.is_empty() || (username.is_empty() && !auth_is_anonymous) {
            self.push_toast(
                "Name, host and username are required",
                ToastVariant::Error,
                cx,
            );
            return;
        }
        if auth_is_key && key_path.is_empty() {
            self.push_toast("A key file is required", ToastVariant::Error, cx);
            return;
        }
        let auth = if auth_is_anonymous {
            AuthMethod::Anonymous
        } else if auth_is_key {
            AuthMethod::Key {
                path: key_path.into(),
            }
        } else {
            AuthMethod::Password
        };
        let port = if port_text.is_empty() {
            protocol.default_port()
        } else {
            match port_text.parse::<u16>() {
                Ok(port) => port,
                Err(_) => {
                    self.push_toast("Port must be a number (1–65535)", ToastVariant::Error, cx);
                    return;
                }
            }
        };

        // Preserve the existing last_connected across an edit.
        let last_connected = self
            .store
            .get(&id)
            .ok()
            .flatten()
            .and_then(|p| p.last_connected);
        let profile = Profile {
            id: id.clone(),
            name,
            protocol,
            ftps_mode,
            host,
            port,
            username,
            auth,
            remote_path: (!remote_path.is_empty()).then_some(remote_path),
            color: color.to_profile_color(),
            last_connected,
        };
        if let Err(err) = self.store.save(&profile) {
            self.push_toast(err.to_string(), ToastVariant::Error, cx);
            return;
        }
        // Persist the secret by method. Anonymous has none - clear any entry left
        // from a prior password/key config so switching modes strands nothing.
        // Blank keeps whatever is already stored (the field shows "leave blank").
        if auth_is_anonymous {
            self.keyring_clear_async(id, cx);
        } else {
            let (secret, account) = if auth_is_key {
                (passphrase, passphrase_account(&id))
            } else {
                (password, password_account(&id))
            };
            if !secret.is_empty() {
                self.keyring_set_async(account, secret, cx);
            }
        }
        self.reload_connections(cx);
        self.editor = None;
        self.push_toast(
            if is_new {
                "Connection created"
            } else {
                "Connection saved"
            },
            ToastVariant::Success,
            cx,
        );
    }

    /// Write a secret to the keychain off-thread, toasting on failure.
    pub(super) fn keyring_set_async(
        &self,
        account: String,
        secret: String,
        cx: &mut Context<Self>,
    ) {
        let keyring = self.keyring;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { keyring.set_password(KEYCHAIN_SERVICE, &account, &secret) })
                .await;
            if res.is_err() {
                this.update(cx, |this, cx| {
                    this.push_toast("Couldn't save secret to keychain", ToastVariant::Error, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Best-effort, idempotent removal of a profile's keychain secrets - both the
    /// password and the key-passphrase entries (a profile may have written either).
    pub(super) fn keyring_clear_async(&self, id: String, cx: &mut Context<Self>) {
        let keyring = self.keyring;
        cx.background_executor()
            .spawn(async move {
                let _ = keyring.delete_password(KEYCHAIN_SERVICE, &password_account(&id));
                let _ = keyring.delete_password(KEYCHAIN_SERVICE, &passphrase_account(&id));
            })
            .detach();
    }

    /// Send a throwaway `TestConnection` probe for the editor's current form. On
    /// edit with a blank password field, the stored secret is fetched first.
    pub fn test_editor_connection(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let name = editor.name.read(cx).content().trim().to_string();
        let host = editor.host.read(cx).content().trim().to_string();
        let port_text = editor.port.read(cx).content().trim().to_string();
        let username = editor.username.read(cx).content().trim().to_string();
        let remote_path = editor.remote_path.read(cx).content().trim().to_string();
        let password = editor.password.read(cx).content().to_string();
        let auth_is_key = editor.auth_is_key;
        let auth_is_anonymous = editor.auth_is_anonymous;
        let key_path = editor.key_path.read(cx).content().trim().to_string();
        let passphrase = editor.passphrase.read(cx).content().to_string();
        let protocol = editor.protocol;
        let ftps_mode = editor.ftps_mode;
        let id = editor.id.clone();
        let is_new = editor.is_new;

        if host.is_empty() || (username.is_empty() && !auth_is_anonymous) {
            self.set_test_status(false, "Host and username are required");
            return;
        }
        if auth_is_key && key_path.is_empty() {
            self.set_test_status(false, "A key file is required");
            return;
        }
        let port = if port_text.is_empty() {
            protocol.default_port()
        } else {
            match port_text.parse::<u16>() {
                Ok(port) => port,
                Err(_) => {
                    self.set_test_status(false, "Port must be a number");
                    return;
                }
            }
        };
        let auth = if auth_is_anonymous {
            AuthMethod::Anonymous
        } else if auth_is_key {
            AuthMethod::Key {
                path: key_path.into(),
            }
        } else {
            AuthMethod::Password
        };
        let profile = Profile {
            id: id.clone(),
            name: if name.is_empty() { "test".into() } else { name },
            protocol,
            ftps_mode,
            host,
            port,
            username,
            auth,
            remote_path: (!remote_path.is_empty()).then_some(remote_path),
            color: ProfileColor::default(),
            last_connected: None,
        };
        if let Some(editor) = self.editor.as_mut() {
            editor.testing = true;
            editor.test_status = None;
        }

        // Anonymous probes with no secret; skip the keychain entirely.
        if auth_is_anonymous {
            self.dispatch_test(profile, String::new());
            cx.notify();
            return;
        }
        // The secret to probe with (password or passphrase). On edit with a blank
        // field, fetch whatever is stored under the matching account.
        let (typed_secret, account) = if auth_is_key {
            (passphrase, passphrase_account(&id))
        } else {
            (password, password_account(&id))
        };
        if typed_secret.is_empty() && !is_new {
            let keyring = self.keyring;
            cx.spawn(async move |this, cx| {
                let res = cx
                    .background_executor()
                    .spawn(async move { keyring.get_password(KEYCHAIN_SERVICE, &account) })
                    .await;
                this.update(cx, |this, cx| {
                    let secret = res.ok().flatten().unwrap_or_default();
                    this.dispatch_test(profile, secret);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        } else {
            self.dispatch_test(profile, typed_secret);
        }
        cx.notify();
    }

    /// Set the editor's inline test status (if the editor is still open).
    pub(super) fn set_test_status(&mut self, ok: bool, message: impl Into<SharedString>) {
        if let Some(editor) = self.editor.as_mut() {
            editor.testing = false;
            editor.test_status = Some(TestStatus {
                ok,
                message: message.into(),
            });
        }
    }

    /// Send the `TestConnection` command, reflecting a send failure inline.
    pub(super) fn dispatch_test(&mut self, profile: Profile, secret: String) {
        let sent = self.service.send(Command::TestConnection {
            profile,
            secret: Secret::new(secret),
        });
        if !sent {
            self.set_test_status(false, "Backend unavailable");
        }
    }
}
