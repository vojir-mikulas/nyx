//! App-managed `known_hosts` store for trust-on-first-use host-key verification.
//!
//! This is intentionally *not* OpenSSH `known_hosts` compatible - V1 only needs a
//! simple, human-inspectable record of "this host presented this fingerprint".
//! Each line is `host fingerprint` (the SHA-256 fingerprint russh exposes, e.g.
//! `SHA256:…`). The file lives under the app's data dir; the path is supplied by
//! the caller (the service wires the per-OS location).
//!
//! Nothing here is a secret - a host fingerprint is public - so this lives in the
//! protocol layer, never the keyring.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

/// The result of checking a presented fingerprint against the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownHostStatus {
    /// The host is recorded and the fingerprint matches - trust it.
    Match,
    /// The host is not recorded - prompt the user (trust-on-first-use).
    Unknown,
    /// The host is recorded but the fingerprint differs - never auto-trust.
    Mismatch,
}

/// A file-backed trust-on-first-use host-key store.
///
/// Cheap to [`Clone`] (just a path); the file is read on each [`check`](Self::check)
/// and appended on [`trust`](Self::trust). For V1's one-connection-at-a-time use
/// that is more than fast enough and keeps the store stateless.
#[derive(Debug, Clone)]
pub struct KnownHosts {
    path: PathBuf,
}

impl KnownHosts {
    /// Build a store backed by the file at `path` (created on first `trust`).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Compare a presented `fingerprint` for `host` against the store.
    ///
    /// A read error (e.g. the file does not exist yet) is treated as "no records",
    /// so an absent file yields [`KnownHostStatus::Unknown`].
    pub fn check(&self, host: &str, fingerprint: &str) -> KnownHostStatus {
        let Ok(contents) = fs::read_to_string(&self.path) else {
            return KnownHostStatus::Unknown;
        };
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(2, ' ');
            let (Some(recorded_host), Some(recorded_fp)) = (parts.next(), parts.next()) else {
                continue;
            };
            if recorded_host == host {
                return if recorded_fp.trim() == fingerprint {
                    KnownHostStatus::Match
                } else {
                    KnownHostStatus::Mismatch
                };
            }
        }
        KnownHostStatus::Unknown
    }

    /// Record `host`/`fingerprint` as trusted, creating the file (and parent dir)
    /// if needed. Appends a single `host fingerprint` line.
    pub fn trust(&self, host: &str, fingerprint: &str) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
            restrict_to_owner(parent, 0o700);
        }
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        // Create the store owner-only: a fingerprint is public, but the list of
        // hosts a user connects to is browsing history worth not leaking.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&self.path)?;
        // `mode` only applies when *this* call creates the file; tighten an
        // already-existing one too (best-effort).
        restrict_to_owner(&self.path, 0o600);
        writeln!(file, "{host} {fingerprint}")
    }
}

/// Best-effort tighten `path` to owner-only on Unix; a no-op elsewhere. A failure
/// is non-fatal - never block recording a trusted host on a perms quirk.
#[cfg(unix)]
fn restrict_to_owner(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &std::path::Path, _mode: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> KnownHosts {
        let mut path = std::env::temp_dir();
        path.push(format!("nyx-known-hosts-test-{name}"));
        let _ = fs::remove_file(&path);
        KnownHosts::at(path)
    }

    #[test]
    fn unknown_then_match_then_mismatch() {
        let store = temp_store("tofu");
        assert_eq!(
            store.check("example.com", "SHA256:aaa"),
            KnownHostStatus::Unknown
        );
        store.trust("example.com", "SHA256:aaa").unwrap();
        assert_eq!(
            store.check("example.com", "SHA256:aaa"),
            KnownHostStatus::Match
        );
        assert_eq!(
            store.check("example.com", "SHA256:bbb"),
            KnownHostStatus::Mismatch
        );
        // A different host is still unknown.
        assert_eq!(
            store.check("other.com", "SHA256:aaa"),
            KnownHostStatus::Unknown
        );
    }

    #[cfg(unix)]
    #[test]
    fn trust_creates_an_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let store = temp_store("perms");
        store.trust("example.com", "SHA256:aaa").unwrap();
        let mode = fs::metadata(&store.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "known_hosts must be owner-only, got {mode:o}");
    }
}
