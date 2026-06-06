// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! OS-keychain credential storage for Nyx.
//!
//! **Security rule:** secrets handled here are *never* logged and *never*
//! written to profile files. They live only in the platform keychain (Keychain
//! on macOS), behind the [`keyring`] crate via [`OsKeyring`].
//!
//! The API is sync and blocking — and on macOS the *first* access pops a system
//! "allow" dialog — so callers run these methods off the UI thread (see the app's
//! `open_connection` / editor flow).

use std::collections::HashMap;
use std::sync::Mutex;

use nyx_core::{NyxError, Result};

/// Read/write access to securely stored credentials.
///
/// Secrets are addressed by `(service, account)`. Implementations must not log
/// or otherwise persist passwords outside the platform secret store.
pub trait CredentialStore {
    /// Fetch a stored password, or `None` if absent.
    fn get_password(&self, service: &str, account: &str) -> Result<Option<String>>;
    /// Store (or replace) a password.
    fn set_password(&self, service: &str, account: &str, password: &str) -> Result<()>;
    /// Delete a stored password (idempotent: a missing entry is success).
    fn delete_password(&self, service: &str, account: &str) -> Result<()>;
}

/// The platform keychain credential store, backed by the `keyring` crate.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct OsKeyring;

impl OsKeyring {
    /// Create a handle to the OS keychain.
    pub fn new() -> Self {
        Self
    }
}

/// Build a `keyring::Entry`, mapping construction errors to a credential-free
/// [`NyxError`].
fn entry(service: &str, account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(service, account).map_err(map_err)
}

/// Map a `keyring` error to a credential-free [`NyxError`]. The `keyring` error
/// `Display` carries only API/status detail — never the password.
fn map_err(err: keyring::Error) -> NyxError {
    NyxError::Other(format!("keychain error: {err}"))
}

impl CredentialStore for OsKeyring {
    fn get_password(&self, service: &str, account: &str) -> Result<Option<String>> {
        match entry(service, account)?.get_password() {
            Ok(password) => Ok(Some(password)),
            // A missing entry is not an error — it means "prompt the user".
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(map_err(err)),
        }
    }

    fn set_password(&self, service: &str, account: &str, password: &str) -> Result<()> {
        entry(service, account)?
            .set_password(password)
            .map_err(map_err)
    }

    fn delete_password(&self, service: &str, account: &str) -> Result<()> {
        match entry(service, account)?.delete_credential() {
            Ok(()) => Ok(()),
            // Deleting an absent entry is a no-op success (idempotent).
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(map_err(err)),
        }
    }
}

/// An in-memory [`CredentialStore`] for tests that must not touch the real
/// keychain (CI has none). Keyed by `(service, account)`.
#[derive(Default)]
pub struct MemoryCredentialStore {
    entries: Mutex<HashMap<(String, String), String>>,
}

impl MemoryCredentialStore {
    /// Create an empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn get_password(&self, service: &str, account: &str) -> Result<Option<String>> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .get(&(service.to_string(), account.to_string()))
            .cloned())
    }

    fn set_password(&self, service: &str, account: &str, password: &str) -> Result<()> {
        self.entries.lock().unwrap().insert(
            (service.to_string(), account.to_string()),
            password.to_string(),
        );
        Ok(())
    }

    fn delete_password(&self, service: &str, account: &str) -> Result<()> {
        self.entries
            .lock()
            .unwrap()
            .remove(&(service.to_string(), account.to_string()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryCredentialStore::new();
        assert_eq!(store.get_password("nyx", "a").unwrap(), None);

        store.set_password("nyx", "a", "hunter2").unwrap();
        assert_eq!(
            store.get_password("nyx", "a").unwrap().as_deref(),
            Some("hunter2")
        );

        // Overwrite.
        store.set_password("nyx", "a", "swordfish").unwrap();
        assert_eq!(
            store.get_password("nyx", "a").unwrap().as_deref(),
            Some("swordfish")
        );

        store.delete_password("nyx", "a").unwrap();
        assert_eq!(store.get_password("nyx", "a").unwrap(), None);
        // Idempotent delete.
        store.delete_password("nyx", "a").unwrap();
    }

    /// Exercises the real OS keychain — ignored by default (CI has none, and it
    /// would pop a system dialog). Run locally with `--ignored`.
    #[test]
    #[ignore = "touches the real OS keychain"]
    fn os_keyring_round_trip() {
        let keyring = OsKeyring::new();
        let account = format!("nyx-test-{}", std::process::id());
        keyring
            .set_password("nyx-test", &account, "secret")
            .unwrap();
        assert_eq!(
            keyring
                .get_password("nyx-test", &account)
                .unwrap()
                .as_deref(),
            Some("secret")
        );
        keyring.delete_password("nyx-test", &account).unwrap();
        assert_eq!(keyring.get_password("nyx-test", &account).unwrap(), None);
    }
}
