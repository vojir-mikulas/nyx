// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! OS-keychain credential storage for Nyx.
//!
//! **Security rule:** secrets handled here are *never* logged and *never*
//! written to profile files. They live only in the platform keychain (Keychain
//! on macOS; the `keyring` crate will back [`OsKeyring`] in a later plan).

use nyx_core::Result;

/// Read/write access to securely stored credentials.
///
/// Secrets are addressed by `(service, account)`. Implementations must not log
/// or otherwise persist passwords outside the platform secret store.
pub trait CredentialStore {
    /// Fetch a stored password, or `None` if absent.
    fn get_password(&self, service: &str, account: &str) -> Result<Option<String>>;
    /// Store (or replace) a password.
    fn set_password(&self, service: &str, account: &str, password: &str) -> Result<()>;
    /// Delete a stored password.
    fn delete_password(&self, service: &str, account: &str) -> Result<()>;
}

/// The platform keychain credential store. Stub — backed by the `keyring` crate
/// in a later plan.
#[derive(Default)]
#[non_exhaustive]
pub struct OsKeyring;

impl OsKeyring {
    /// Create a handle to the OS keychain.
    pub fn new() -> Self {
        Self
    }
}

impl CredentialStore for OsKeyring {
    fn get_password(&self, _service: &str, _account: &str) -> Result<Option<String>> {
        unimplemented!("OS keychain access is not implemented yet")
    }

    fn set_password(&self, _service: &str, _account: &str, _password: &str) -> Result<()> {
        unimplemented!("OS keychain access is not implemented yet")
    }

    fn delete_password(&self, _service: &str, _account: &str) -> Result<()> {
        unimplemented!("OS keychain access is not implemented yet")
    }
}
