//! Persisted UI preferences (theme, row density, permissions column).
//!
//! These are app-wide presentation settings, not per-connection config, so they
//! live in their own `settings.toml` next to `profiles.toml`. Like
//! [`ProfileColor`](crate::ProfileColor), the values are stored as
//! provider-agnostic primitives (a theme *name*, a density *index*) and mapped
//! to concrete UI types in the app - `nyx-profile` stays UI-free.
//!
//! Unlike the profile store, a missing **or malformed** file falls back to
//! [`Settings::default`]: preferences are convenience, not user data, so a bad
//! file should never block startup or get surfaced as an error.

use std::fs;
use std::path::{Path, PathBuf};

use nyx_core::{NyxError, Result};
use serde::{Deserialize, Serialize};

/// Persisted UI preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The active theme's human-readable name (e.g. `"One Dark"`); mapped to a
    /// concrete `Theme` in the app.
    pub theme: String,
    /// File-row density as an index into the app's density list (0/1/2).
    pub density: u8,
    /// Whether the browser's permissions column is shown.
    pub show_perms: bool,
    /// Whether a dropped session reconnects automatically (with backoff) before
    /// falling back to a manual reconnect.
    pub auto_reconnect: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "One Dark".to_string(),
            density: 1,
            show_perms: true,
            auto_reconnect: true,
        }
    }
}

/// Local on-disk settings store over a single `settings.toml`.
///
/// Mirrors [`FileProfileStore`](crate::FileProfileStore)'s atomic write (temp
/// file + rename), but reads never fail: a missing or malformed file yields
/// [`Settings::default`].
#[derive(Debug, Clone)]
pub struct FileSettingsStore {
    path: PathBuf,
}

impl FileSettingsStore {
    /// Open the store at the per-OS config location
    /// (`<config_dir>/settings.toml`), matching the profile store's identity.
    pub fn open_default() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("dev", "nyx", "Nyx")
            .ok_or_else(|| NyxError::Other("could not determine the OS config directory".into()))?;
        Ok(Self::with_path(dirs.config_dir().join("settings.toml")))
    }

    /// Open a store backed by an explicit file path (used in tests).
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The path to the backing file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the settings, falling back to [`Settings::default`] for a missing or
    /// malformed file (preferences must never block startup).
    pub fn load(&self) -> Settings {
        match fs::read_to_string(&self.path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    /// Serialize and write atomically: a sibling temp file then a rename over the
    /// target (atomic on the same volume).
    pub fn save(&self, settings: &Settings) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|err| NyxError::Io(err.to_string()))?;
        }
        let serialized =
            toml::to_string_pretty(settings).map_err(|err| NyxError::Other(err.to_string()))?;

        let tmp = self.path.with_extension("toml.tmp");
        fs::write(&tmp, serialized).map_err(|err| NyxError::Io(err.to_string()))?;
        fs::rename(&tmp, &self.path).map_err(|err| NyxError::Io(err.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, FileSettingsStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSettingsStore::with_path(dir.path().join("settings.toml"));
        (dir, store)
    }

    #[test]
    fn missing_file_is_default() {
        let (_dir, store) = temp_store();
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn round_trip() {
        let (_dir, store) = temp_store();
        let settings = Settings {
            theme: "Ayu Dark".to_string(),
            density: 0,
            show_perms: false,
            auto_reconnect: false,
        };
        store.save(&settings).unwrap();
        assert_eq!(store.load(), settings);
    }

    #[test]
    fn malformed_file_is_default() {
        let (_dir, store) = temp_store();
        fs::write(store.path(), "this is = not valid toml ][").unwrap();
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn partial_file_takes_field_defaults() {
        // A file with only `theme` set keeps the default density / show_perms.
        let (_dir, store) = temp_store();
        fs::write(store.path(), "theme = \"GitHub Dark\"\n").unwrap();
        let loaded = store.load();
        assert_eq!(loaded.theme, "GitHub Dark");
        assert_eq!(loaded.density, Settings::default().density);
        assert_eq!(loaded.show_perms, Settings::default().show_perms);
    }
}
