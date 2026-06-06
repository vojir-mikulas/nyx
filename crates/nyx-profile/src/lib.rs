// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! Connection profiles for Nyx.
//!
//! A [`Profile`] is the persisted, shareable description of a connection — host,
//! port, protocol, username, default path. **Credentials are never stored here**;
//! passwords live in the OS keychain (see `nyx-keyring`). The [`ProfileStore`]
//! trait abstracts persistence; [`FileProfileStore`] is the local on-disk
//! implementation (stub for now).

use nyx_core::{Protocol, Result};
use serde::{Deserialize, Serialize};

/// A saved connection profile (no secrets).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable identifier (e.g. a UUID string).
    pub id: String,
    /// Human-friendly display name.
    pub name: String,
    /// Which protocol to use.
    pub protocol: Protocol,
    /// Remote hostname or IP.
    pub host: String,
    /// Remote port.
    pub port: u16,
    /// Login username.
    pub username: String,
    /// Directory to open on connect, if any.
    pub remote_path: Option<String>,
}

/// Persistence for [`Profile`]s.
pub trait ProfileStore {
    /// All stored profiles.
    fn list(&self) -> Result<Vec<Profile>>;
    /// Fetch a single profile by id.
    fn get(&self, id: &str) -> Result<Option<Profile>>;
    /// Create or update a profile.
    fn save(&mut self, profile: &Profile) -> Result<()>;
    /// Remove a profile by id.
    fn delete(&mut self, id: &str) -> Result<()>;
}

/// Local on-disk profile store. Stub — persistence lands in a later plan.
#[derive(Default)]
#[non_exhaustive]
pub struct FileProfileStore {
    // Path to the profiles file / directory goes here.
}

impl FileProfileStore {
    /// Create a new file-backed profile store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ProfileStore for FileProfileStore {
    fn list(&self) -> Result<Vec<Profile>> {
        unimplemented!("profile persistence is not implemented yet")
    }

    fn get(&self, _id: &str) -> Result<Option<Profile>> {
        unimplemented!("profile persistence is not implemented yet")
    }

    fn save(&mut self, _profile: &Profile) -> Result<()> {
        unimplemented!("profile persistence is not implemented yet")
    }

    fn delete(&mut self, _id: &str) -> Result<()> {
        unimplemented!("profile persistence is not implemented yet")
    }
}
