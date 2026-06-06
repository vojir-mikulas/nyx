//! Connection profiles for Nyx.
//!
//! A [`Profile`] is the persisted, shareable description of a connection — host,
//! port, protocol, username, default path, plus two presentation fields that
//! travel with it (accent [`color`](Profile::color) and a `last_connected`
//! timestamp). **Credentials are never stored here**; passwords live in the OS
//! keychain (see `nyx-keyring`). The [`ProfileStore`] trait abstracts
//! persistence; [`FileProfileStore`] is the local on-disk implementation over a
//! single TOML file.

use std::fs;
use std::path::{Path, PathBuf};

use nyx_core::{NyxError, Protocol, Result};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

mod settings;
pub use settings::{FileSettingsStore, Settings};

/// A saved connection profile (no secrets).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable identifier (a UUID v4 string; see [`Profile::new_id`]).
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
    /// How this connection authenticates (password or private key).
    ///
    /// `#[serde(default)]` so profiles written before this field existed still
    /// parse — they take [`AuthMethod::Password`]. The key *path* lives here (it
    /// is not a secret); the password / key passphrase lives in the keychain.
    #[serde(default)]
    pub auth: AuthMethod,
    /// Directory to open on connect, if any.
    pub remote_path: Option<String>,
    /// The accent color shown for this connection (presentation, persisted).
    ///
    /// Stored here — not on `nyx-core` — because it is config that travels with
    /// the saved connection, not a protocol invariant. Mapped to a UI accent in
    /// the app.
    #[serde(default)]
    pub color: ProfileColor,
    /// When this profile was last successfully connected to, if ever.
    ///
    /// Drives the sidebar's "Recent" ordering. `#[serde(default)]` so older /
    /// hand-written files (without the field) still load.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_connected: Option<OffsetDateTime>,
}

/// How a profile authenticates to its remote host.
///
/// Internally tagged on `method` so the TOML reads naturally:
/// ```toml
/// [profile.auth]
/// method = "key"
/// path = "/home/me/.ssh/id_ed25519"
/// ```
/// The secret (password or key passphrase) is **never** stored here — only the
/// non-secret key *path* travels with the profile.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "lowercase")]
pub enum AuthMethod {
    /// Password authentication (the default, and the pre-`auth`-field behavior).
    #[default]
    Password,
    /// Public-key authentication with an OpenSSH private key file.
    Key {
        /// Path to the private key file (non-secret, persisted).
        path: PathBuf,
    },
}

impl Profile {
    /// Generate a fresh, collision-free profile id (UUID v4 as a string).
    ///
    /// The id is the store key *and* the keychain account; it is generated once
    /// on create and never reused or mutated.
    pub fn new_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

/// A connection's accent color (presentation, persisted with the profile).
///
/// Owned here — not pulled from `nyx-ui` — so `nyx-profile` keeps zero UI
/// coupling; the app maps this to a concrete theme accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileColor {
    /// Blue accent.
    Blue,
    /// Purple accent (the default — matches SFTP, the V1 protocol).
    #[default]
    Purple,
    /// Green accent.
    Green,
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

/// The on-disk TOML wrapper: a single `[[profile]]` table-array.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    profile: Vec<Profile>,
}

/// Local on-disk profile store over a single `profiles.toml`.
///
/// The whole file is read on each read and rewritten atomically (temp file +
/// rename) on each mutation. N is tiny, so one file keeps the store simple to
/// inspect and impossible to half-write.
#[derive(Debug, Clone)]
pub struct FileProfileStore {
    path: PathBuf,
}

impl FileProfileStore {
    /// Open the store at the per-OS config location
    /// (`<config_dir>/profiles.toml`, resolved via the `directories` crate).
    pub fn open_default() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("dev", "nyx", "Nyx")
            .ok_or_else(|| NyxError::Other("could not determine the OS config directory".into()))?;
        Ok(Self::with_path(dirs.config_dir().join("profiles.toml")))
    }

    /// Open a store backed by an explicit file path (used in tests).
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The path to the backing file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read and parse the whole file. A missing file is an empty list (first
    /// run); a *malformed* file is a clear error naming the path, so the user's
    /// connections are never silently dropped.
    fn load(&self) -> Result<Vec<Profile>> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(NyxError::Io(err.to_string())),
        };
        let parsed: ProfilesFile = toml::from_str(&contents).map_err(|err| {
            NyxError::Other(format!(
                "malformed profile store at {}: {err}",
                self.path.display()
            ))
        })?;
        Ok(parsed.profile)
    }

    /// Serialize and write the whole list atomically: a sibling temp file then a
    /// rename over the target (atomic on the same volume), so a crash mid-write
    /// can't corrupt the list.
    fn store(&self, profiles: &[Profile]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|err| NyxError::Io(err.to_string()))?;
        }
        let file = ProfilesFile {
            profile: profiles.to_vec(),
        };
        let serialized =
            toml::to_string_pretty(&file).map_err(|err| NyxError::Other(err.to_string()))?;

        let tmp = self.path.with_extension("toml.tmp");
        fs::write(&tmp, serialized).map_err(|err| NyxError::Io(err.to_string()))?;
        fs::rename(&tmp, &self.path).map_err(|err| NyxError::Io(err.to_string()))?;
        Ok(())
    }
}

impl ProfileStore for FileProfileStore {
    fn list(&self) -> Result<Vec<Profile>> {
        self.load()
    }

    fn get(&self, id: &str) -> Result<Option<Profile>> {
        Ok(self.load()?.into_iter().find(|p| p.id == id))
    }

    fn save(&mut self, profile: &Profile) -> Result<()> {
        let mut profiles = self.load()?;
        match profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(existing) => *existing = profile.clone(),
            None => profiles.push(profile.clone()),
        }
        self.store(&profiles)
    }

    fn delete(&mut self, id: &str) -> Result<()> {
        let mut profiles = self.load()?;
        profiles.retain(|p| p.id != id);
        self.store(&profiles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str, name: &str) -> Profile {
        Profile {
            id: id.to_string(),
            name: name.to_string(),
            protocol: Protocol::Sftp,
            host: "example.com".to_string(),
            port: 22,
            username: "deploy".to_string(),
            auth: AuthMethod::Password,
            remote_path: Some("/var/www".to_string()),
            color: ProfileColor::Green,
            last_connected: None,
        }
    }

    fn temp_store() -> (tempfile::TempDir, FileProfileStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileProfileStore::with_path(dir.path().join("profiles.toml"));
        (dir, store)
    }

    #[test]
    fn missing_file_is_empty_list() {
        let (_dir, store) = temp_store();
        assert!(store.list().unwrap().is_empty());
        assert!(store.get("nope").unwrap().is_none());
    }

    #[test]
    fn crud_round_trip() {
        let (_dir, mut store) = temp_store();

        store.save(&sample("a", "alpha")).unwrap();
        store.save(&sample("b", "bravo")).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);

        // Upsert by id rather than duplicating.
        store.save(&sample("a", "alpha-renamed")).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
        assert_eq!(store.get("a").unwrap().unwrap().name, "alpha-renamed");

        store.delete("a").unwrap();
        let remaining = store.list().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "b");

        // Deleting a non-existent id is a no-op, not an error.
        store.delete("ghost").unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn serialized_file_has_no_secret() {
        let (_dir, mut store) = temp_store();
        store.save(&sample("a", "alpha")).unwrap();
        let on_disk = fs::read_to_string(store.path()).unwrap();
        let lower = on_disk.to_lowercase();
        // `method = "password"` (the auth *method* name) is expected; what must
        // never appear is a secret value — a stored password/passphrase field.
        assert!(!lower.contains("passphrase"));
        assert!(!lower.contains("password ="));
        assert!(!lower.contains("secret"));
    }

    #[test]
    fn minimal_profile_loads_with_defaults() {
        // A hand-written profile missing `color` / `last_connected` must load,
        // taking the serde defaults.
        let (_dir, store) = temp_store();
        let minimal = r#"
            [[profile]]
            id = "x"
            name = "minimal"
            protocol = "sftp"
            host = "h"
            port = 22
            username = "u"
        "#;
        fs::write(store.path(), minimal).unwrap();
        let loaded = store.get("x").unwrap().unwrap();
        assert_eq!(loaded.color, ProfileColor::default());
        assert!(loaded.last_connected.is_none());
        assert!(loaded.remote_path.is_none());
    }

    #[test]
    fn malformed_file_is_an_error() {
        let (_dir, store) = temp_store();
        fs::write(store.path(), "this is = not valid toml ][").unwrap();
        let err = store.list().unwrap_err();
        assert!(err.to_string().contains("malformed profile store"));
    }

    #[test]
    fn auth_method_round_trips() {
        let (_dir, mut store) = temp_store();
        let mut p = sample("k", "keyed");
        p.auth = AuthMethod::Key {
            path: PathBuf::from("/home/me/.ssh/id_ed25519"),
        };
        store.save(&p).unwrap();
        assert_eq!(store.get("k").unwrap().unwrap().auth, p.auth);

        // The default (Password) variant also survives a round-trip.
        store.save(&sample("p", "passworded")).unwrap();
        assert_eq!(store.get("p").unwrap().unwrap().auth, AuthMethod::Password);
    }

    #[test]
    fn missing_auth_field_defaults_to_password() {
        // A profile written before `auth` existed must load as Password.
        let (_dir, store) = temp_store();
        let legacy = r#"
            [[profile]]
            id = "x"
            name = "legacy"
            protocol = "sftp"
            host = "h"
            port = 22
            username = "u"
        "#;
        fs::write(store.path(), legacy).unwrap();
        assert_eq!(store.get("x").unwrap().unwrap().auth, AuthMethod::Password);
    }

    #[test]
    fn key_path_is_not_a_secret_on_disk() {
        // The key path is fine to persist; no secret ever is.
        let (_dir, mut store) = temp_store();
        let mut p = sample("k", "keyed");
        p.auth = AuthMethod::Key {
            path: PathBuf::from("/home/me/.ssh/id_ed25519"),
        };
        store.save(&p).unwrap();
        let on_disk = fs::read_to_string(store.path()).unwrap();
        assert!(on_disk.contains("id_ed25519"));
        let lower = on_disk.to_lowercase();
        assert!(!lower.contains("passphrase"));
        assert!(!lower.contains("password"));
    }

    #[test]
    fn timestamp_round_trips() {
        let (_dir, mut store) = temp_store();
        let mut p = sample("t", "timed");
        p.last_connected = Some(OffsetDateTime::from_unix_timestamp(1_749_200_040).unwrap());
        store.save(&p).unwrap();
        assert_eq!(
            store.get("t").unwrap().unwrap().last_connected,
            p.last_connected
        );
    }
}
