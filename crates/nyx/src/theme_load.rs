//! User-authored themes: load `*.toml` files from the config `themes/` dir and
//! merge them onto a built-in base.
//!
//! The on-disk format and its `gpui`/[`Theme`] conversion live here, in the app,
//! *not* in `nyx-profile` (which stays UI-free) or `nyx-ui` (which stays
//! serde-free for the Flint extraction). A theme is base + overrides: a file only
//! names the tokens it changes, so a forgotten key inherits from `base` rather
//! than rendering black. A malformed file is skipped with a `warn!`, never an
//! error - one bad theme must not break the picker.

use std::fs;
use std::path::{Path, PathBuf};

use gpui::{px, rgb, rgba, Hsla};
use nyx_ui::Theme;
use serde::Deserialize;

/// The built-in themes, in picker order. The first is the fallback default.
fn builtins() -> Vec<Theme> {
    vec![
        Theme::one_dark(),
        Theme::github_dark(),
        Theme::ayu_dark(),
        Theme::ayu_light(),
    ]
}

/// A built-in by name, used to resolve a theme file's `base`.
fn builtin_by_name(name: &str) -> Option<Theme> {
    builtins().into_iter().find(|t| t.name == name)
}

/// Where a theme came from: a compiled-in built-in, or a user file on disk.
#[derive(Clone)]
enum ThemeSource {
    Builtin,
    User(PathBuf),
}

#[derive(Clone)]
struct ThemeEntry {
    theme: Theme,
    source: ThemeSource,
}

/// A theme's identity for the manager UI: its name and whether it's removable.
pub struct ThemeInfo {
    pub name: String,
    pub custom: bool,
}

/// The active set of themes: built-ins followed by valid user themes. Owned by
/// `AppState`, rebuilt whenever a theme is added or removed.
#[derive(Clone)]
pub struct ThemeRegistry {
    entries: Vec<ThemeEntry>,
}

impl ThemeRegistry {
    /// Build the registry: the built-ins, then every well-formed `*.toml` in the
    /// config `themes/` dir whose name doesn't collide with an existing one.
    pub fn load() -> Self {
        let mut entries: Vec<ThemeEntry> = builtins()
            .into_iter()
            .map(|theme| ThemeEntry {
                theme,
                source: ThemeSource::Builtin,
            })
            .collect();
        for (path, theme) in load_user_themes() {
            if entries.iter().any(|e| e.theme.name == theme.name) {
                tracing::warn!(
                    name = %theme.name,
                    "ignoring user theme: its name collides with an existing theme"
                );
                continue;
            }
            entries.push(ThemeEntry {
                theme,
                source: ThemeSource::User(path),
            });
        }
        Self { entries }
    }

    /// Every theme in picker order, tagged built-in vs. custom.
    pub fn list(&self) -> Vec<ThemeInfo> {
        self.entries
            .iter()
            .map(|e| ThemeInfo {
                name: e.theme.name.clone(),
                custom: matches!(e.source, ThemeSource::User(_)),
            })
            .collect()
    }

    /// Resolve a name to its [`Theme`], falling back to the default built-in for
    /// an unknown name (a user theme that was renamed or removed).
    pub fn by_name(&self, name: &str) -> Theme {
        self.entries
            .iter()
            .find(|e| e.theme.name == name)
            .map(|e| e.theme.clone())
            .unwrap_or_else(Theme::one_dark)
    }

    /// Delete a custom theme's file from disk. Errors for a built-in (which has no
    /// file) or an unknown name. The caller reloads the registry afterwards.
    pub fn remove(&self, name: &str) -> Result<(), String> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.theme.name == name)
            .ok_or_else(|| format!("no theme named “{name}”"))?;
        match &entry.source {
            ThemeSource::Builtin => Err("built-in themes can't be removed".to_string()),
            ThemeSource::User(path) => {
                fs::remove_file(path).map_err(|err| format!("can't delete theme file: {err}"))
            }
        }
    }
}

/// Read and parse every `*.toml` under the config `themes/` dir. A missing dir
/// (the common case) is an empty list; a malformed file is skipped with a warn.
fn load_user_themes() -> Vec<(PathBuf, Theme)> {
    let Some(dir) = themes_dir() else {
        return Vec::new();
    };
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(dir = %dir.display(), %err, "could not read the themes directory");
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        match load_theme_file(&path) {
            Ok(theme) => out.push((path, theme)),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "skipping malformed theme file")
            }
        }
    }
    out
}

/// The per-OS config `themes/` directory (`<config_dir>/themes`), matching the
/// profile/settings store identity.
fn themes_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "nyx", "Nyx").map(|dirs| dirs.config_dir().join("themes"))
}

/// Parse one theme file and merge it onto its base.
fn load_theme_file(path: &Path) -> Result<Theme, String> {
    let contents = fs::read_to_string(path).map_err(|err| err.to_string())?;
    parse_theme(&contents)
}

/// Validate a theme file the user picked and copy it into the config `themes/`
/// dir so it persists and joins the picker. Returns the installed theme's name on
/// success; the `Err` string is suitable for a toast.
///
/// Validation reuses [`parse_theme`], so a bad file is rejected *before* anything
/// is written. The destination name is a slug of the theme name, so re-importing
/// an edited theme overwrites rather than duplicates.
pub fn install_theme(src: &Path) -> Result<String, String> {
    let contents = fs::read_to_string(src).map_err(|err| format!("can't read file: {err}"))?;
    let theme = parse_theme(&contents)?;

    if builtin_by_name(&theme.name).is_some() {
        return Err(format!(
            "\u{201c}{}\u{201d} is a built-in theme name; rename yours",
            theme.name
        ));
    }

    let dir = themes_dir().ok_or("could not determine the config directory")?;
    fs::create_dir_all(&dir).map_err(|err| format!("can't create themes folder: {err}"))?;
    let dest = dir.join(format!("{}.toml", slug(&theme.name)));
    fs::write(&dest, &contents).map_err(|err| format!("can't save theme: {err}"))?;

    Ok(theme.name)
}

/// A filesystem-safe slug for a theme name (`"My Theme!" → "my-theme"`).
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() {
        "theme".to_string()
    } else {
        slug
    }
}

/// Parse a theme document and apply it onto the named (or default) base.
fn parse_theme(contents: &str) -> Result<Theme, String> {
    let file: ThemeFile = toml::from_str(contents).map_err(|err| err.to_string())?;
    let base = match &file.base {
        Some(name) => {
            builtin_by_name(name).ok_or_else(|| format!("unknown base theme `{name}`"))?
        }
        None => Theme::one_dark(),
    };
    Ok(file.into_theme(base))
}

/// The on-disk theme document. `name` is required; everything else overrides the
/// base. `deny_unknown_fields` on the override tables turns a misspelled token
/// (`acccent`) into a loud error instead of a silent no-op.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    name: String,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    colors: ColorOverrides,
    #[serde(default)]
    layout: LayoutOverrides,
}

macro_rules! color_overrides {
    ($($field:ident),+ $(,)?) => {
        #[derive(Default, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ColorOverrides {
            $( #[serde(default)] $field: Option<HexColor>, )+
        }

        impl ColorOverrides {
            fn apply(self, theme: &mut Theme) {
                $( if let Some(c) = self.$field { theme.$field = c.0; } )+
            }
        }
    };
}

color_overrides! {
    bg_app, bg_panel, bg_panel_2, bg_elevated, bg_bar, bg_hover, bg_active,
    bg_selected, bg_input,
    border, border_soft, border_strong,
    text, text_muted, text_faint, text_dim,
    accent, accent_hover, accent_ghost, on_accent,
    green, red, blue, purple, yellow, orange,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutOverrides {
    #[serde(default)]
    row_height: Option<f32>,
    #[serde(default)]
    radius: Option<f32>,
    #[serde(default)]
    radius_sm: Option<f32>,
}

impl ThemeFile {
    fn into_theme(self, mut base: Theme) -> Theme {
        base.name = self.name;
        self.colors.apply(&mut base);
        if let Some(v) = self.layout.row_height {
            base.row_height = px(v);
        }
        if let Some(v) = self.layout.radius {
            base.radius = px(v);
        }
        if let Some(v) = self.layout.radius_sm {
            base.radius_sm = px(v);
        }
        base
    }
}

/// A color parsed from a `#RGB` / `#RRGGBB` / `#RRGGBBAA` hex string. The 8-digit
/// form carries alpha, covering the translucent tokens (e.g. `accent_ghost`).
struct HexColor(Hsla);

impl<'de> Deserialize<'de> for HexColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_hex(&s)
            .map(HexColor)
            .map_err(serde::de::Error::custom)
    }
}

fn parse_hex(s: &str) -> Result<Hsla, String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("`{s}` is not a hex color"));
    }
    match hex.len() {
        3 => {
            // #RGB → #RRGGBB (double each nibble).
            let expanded: String = hex.chars().flat_map(|c| [c, c]).collect();
            let v = u32::from_str_radix(&expanded, 16).map_err(|e| e.to_string())?;
            Ok(rgb(v).into())
        }
        6 => {
            let v = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
            Ok(rgb(v).into())
        }
        8 => {
            let v = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
            Ok(rgba(v).into())
        }
        _ => Err(format!("`{s}` must be #RGB, #RRGGBB, or #RRGGBBAA")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_forms_parse() {
        assert_eq!(parse_hex("#fff").unwrap(), rgb(0xffffff).into());
        assert_eq!(parse_hex("#282c33").unwrap(), rgb(0x282c33).into());
        assert_eq!(parse_hex("282c33").unwrap(), rgb(0x282c33).into());
        assert_eq!(parse_hex("#98c37929").unwrap(), rgba(0x98c37929).into());
    }

    #[test]
    fn bad_hex_is_rejected() {
        assert!(parse_hex("#zzz").is_err());
        assert!(parse_hex("#12345").is_err());
        assert!(parse_hex("").is_err());
    }

    #[test]
    fn overrides_merge_onto_base() {
        let theme = parse_theme(
            r##"
            name = "Mine"
            base = "One Dark"
            [colors]
            accent = "#ff0000"
            [layout]
            radius = 8.0
            "##,
        )
        .unwrap();

        assert_eq!(theme.name, "Mine");
        assert_eq!(theme.accent, rgb(0xff0000).into());
        assert_eq!(theme.radius, px(8.0));
        // Untouched tokens inherit from the base.
        assert_eq!(theme.bg_app, Theme::one_dark().bg_app);
        assert_eq!(theme.radius_sm, Theme::one_dark().radius_sm);
    }

    #[test]
    fn base_defaults_to_one_dark() {
        let theme = parse_theme("name = \"Bare\"").unwrap();
        assert_eq!(theme.bg_app, Theme::one_dark().bg_app);
    }

    #[test]
    fn unknown_base_is_an_error() {
        let err = parse_theme("name = \"X\"\nbase = \"Nope\"").unwrap_err();
        assert!(err.contains("unknown base theme"));
    }

    #[test]
    fn misspelled_token_is_an_error() {
        // `deny_unknown_fields` catches authoring typos rather than ignoring them.
        let err = parse_theme("name = \"X\"\n[colors]\nacccent = \"#fff\"").unwrap_err();
        assert!(err.to_lowercase().contains("acccent") || err.contains("unknown field"));
    }

    #[test]
    fn missing_name_is_an_error() {
        assert!(parse_theme("[colors]\naccent = \"#fff\"").is_err());
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(slug("My Theme"), "my-theme");
        assert_eq!(slug("  Tokyo Night!! "), "tokyo-night");
        assert_eq!(slug("One/Dark"), "one-dark");
        assert_eq!(slug("***"), "theme");
    }
}
