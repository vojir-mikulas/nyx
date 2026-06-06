// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! Where the app-managed `known_hosts` file lives.
//!
//! M2 uses a fixed path under the user's config dir. M3 replaces this with the
//! `directories` crate's per-OS data dir (the same move planned for the profile
//! store), at which point this module goes away.

use std::path::PathBuf;

/// The path to the trust-on-first-use `known_hosts` store.
///
/// `~/.config/nyx/known_hosts`, falling back to the current dir if `$HOME` is
/// unset (never expected in practice). Replaced by `directories` in M3.
pub fn known_hosts() -> PathBuf {
    let mut path = home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(".config");
    path.push("nyx");
    path.push("known_hosts");
    path
}

/// Best-effort home directory without pulling in a dependency (M2 only).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
