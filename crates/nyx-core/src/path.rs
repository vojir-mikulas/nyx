//! [`RemotePath`] — the canonical remote-path newtype.
//!
//! The invariant — **absolute, `/`-rooted, normalized** (no empty / `.` / `..`
//! components, no repeated or trailing slashes) — is enforced **once at
//! construction** via [`normalize`] and trusted by every operation. This keeps
//! navigation, breadcrumbs and the transfer specs keyed on one canonical string
//! instead of ad-hoc `format!`/`join` math at each call site.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// An absolute, `/`-rooted, normalized remote path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemotePath(String);

/// Why [`RemotePath::try_new`] rejected an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePathError {
    /// The input contained a NUL or other control character.
    ControlChar,
}

impl fmt::Display for RemotePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemotePathError::ControlChar => f.write_str("path contains a control character"),
        }
    }
}

impl std::error::Error for RemotePathError {}

impl RemotePath {
    /// The filesystem root, `/`.
    pub fn root() -> Self {
        Self("/".to_string())
    }

    /// Construct a normalized path, **sanitizing** the input: control characters
    /// are stripped and a non-absolute input is rooted. Never fails — use
    /// [`RemotePath::try_new`] when control characters should be rejected
    /// instead (e.g. a path echoed back by a server).
    pub fn new(s: impl AsRef<str>) -> Self {
        let cleaned: String = s.as_ref().chars().filter(|c| !c.is_control()).collect();
        Self(normalize(&cleaned))
    }

    /// Construct a normalized path, rejecting any input that contains a control
    /// character (NUL included) rather than silently stripping it.
    pub fn try_new(s: impl AsRef<str>) -> Result<Self, RemotePathError> {
        let s = s.as_ref();
        if s.chars().any(|c| c.is_control()) {
            return Err(RemotePathError::ControlChar);
        }
        Ok(Self(normalize(s)))
    }

    /// Append `segment` as a child, normalizing the result (so `join("..")` pops
    /// the last component and `join("a/b")` is allowed and normalized).
    pub fn join(&self, segment: &str) -> RemotePath {
        // At root `self.0` is "/", so this yields "//seg" which normalizes to
        // "/seg"; elsewhere "/a/b" + "c" → "/a/b/c".
        RemotePath::new(format!("{}/{}", self.0, segment))
    }

    /// The parent directory, or `None` at the root.
    pub fn parent(&self) -> Option<RemotePath> {
        if self.is_root() {
            return None;
        }
        // The invariant guarantees a leading '/', no trailing '/'.
        match self.0.rsplit_once('/') {
            Some(("", _)) => Some(RemotePath::root()),
            Some((parent, _)) => Some(RemotePath(parent.to_string())),
            None => None,
        }
    }

    /// The last component (the file/folder name), or `None` at the root.
    pub fn file_name(&self) -> Option<&str> {
        if self.is_root() {
            return None;
        }
        self.0.rsplit('/').next()
    }

    /// The path's components, root-first, with no empty entries (for breadcrumbs).
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|s| !s.is_empty())
    }

    /// The canonical path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the root path, `/`.
    pub fn is_root(&self) -> bool {
        self.0 == "/"
    }

    /// Whether `self` is `base` itself or a descendant of it, comparing whole
    /// path components (so `/a/b` is within `/a` but `/ab` is not). The root
    /// contains everything. Used by the transfer path-lock to make a directory
    /// transfer cover its subtree.
    pub fn is_within(&self, base: &RemotePath) -> bool {
        if base.is_root() {
            return true;
        }
        self.0 == base.0 || self.0.starts_with(&format!("{}/", base.0))
    }
}

/// Normalize a path's structure: collapse repeated slashes, drop `.`, resolve
/// `..` (clamping at root), strip the trailing slash, and root a relative input.
/// This is the single source of truth the tests pin.
fn normalize(input: &str) -> String {
    let mut comps: Vec<&str> = Vec::new();
    for comp in input.split('/') {
        match comp {
            "" | "." => continue,
            ".." => {
                comps.pop();
            }
            other => comps.push(other),
        }
    }
    if comps.is_empty() {
        return "/".to_string();
    }
    let mut out = String::with_capacity(input.len() + 1);
    for comp in comps {
        out.push('/');
        out.push_str(comp);
    }
    out
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RemotePath {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(s))
    }
}

impl From<&str> for RemotePath {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for RemotePath {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_repeated_slashes() {
        assert_eq!(RemotePath::new("//").as_str(), "/");
        assert_eq!(RemotePath::new("/home//user").as_str(), "/home/user");
        assert_eq!(RemotePath::new("/a///b////c").as_str(), "/a/b/c");
    }

    #[test]
    fn strips_trailing_slash_except_root() {
        assert_eq!(RemotePath::new("/home/").as_str(), "/home");
        assert_eq!(RemotePath::new("/").as_str(), "/");
        assert_eq!(RemotePath::new("/a/b/").as_str(), "/a/b");
    }

    #[test]
    fn resolves_dot_and_dotdot() {
        assert_eq!(RemotePath::new("/a/./b").as_str(), "/a/b");
        assert_eq!(RemotePath::new("/a/b/..").as_str(), "/a");
        assert_eq!(RemotePath::new("/a/b/../c").as_str(), "/a/c");
        // Popping past root clamps at "/".
        assert_eq!(RemotePath::new("/..").as_str(), "/");
        assert_eq!(RemotePath::new("/a/../../..").as_str(), "/");
    }

    #[test]
    fn roots_empty_and_relative_input() {
        assert_eq!(RemotePath::new("").as_str(), "/");
        assert_eq!(RemotePath::new("home/user").as_str(), "/home/user");
        assert_eq!(RemotePath::new(".").as_str(), "/");
    }

    #[test]
    fn new_sanitizes_control_chars_try_new_rejects() {
        assert_eq!(RemotePath::new("/a\0b").as_str(), "/ab");
        assert_eq!(RemotePath::new("/a\tb\n").as_str(), "/ab");
        assert_eq!(
            RemotePath::try_new("/a\0b"),
            Err(RemotePathError::ControlChar)
        );
        assert_eq!(RemotePath::try_new("/a/b").unwrap().as_str(), "/a/b");
    }

    #[test]
    fn root_helpers() {
        assert!(RemotePath::root().is_root());
        assert_eq!(RemotePath::root().as_str(), "/");
        assert_eq!(RemotePath::root().parent(), None);
        assert_eq!(RemotePath::root().file_name(), None);
        assert_eq!(RemotePath::root().components().count(), 0);
    }

    #[test]
    fn join_appends_and_normalizes() {
        assert_eq!(RemotePath::root().join("home").as_str(), "/home");
        assert_eq!(RemotePath::new("/a").join("b").as_str(), "/a/b");
        assert_eq!(RemotePath::new("/a/b").join("..").as_str(), "/a");
        assert_eq!(RemotePath::new("/a").join("b/c").as_str(), "/a/b/c");
    }

    #[test]
    fn parent_and_file_name() {
        let p = RemotePath::new("/a/b/c");
        assert_eq!(p.file_name(), Some("c"));
        assert_eq!(p.parent().unwrap().as_str(), "/a/b");
        assert_eq!(RemotePath::new("/foo").parent().unwrap().as_str(), "/");
        assert_eq!(RemotePath::new("/foo").file_name(), Some("foo"));
    }

    #[test]
    fn components_round_trip() {
        let p = RemotePath::new("/var/www/html");
        let comps: Vec<&str> = p.components().collect();
        assert_eq!(comps, vec!["var", "www", "html"]);
    }

    #[test]
    fn is_within_compares_whole_components() {
        let base = RemotePath::new("/a/b");
        assert!(RemotePath::new("/a/b").is_within(&base)); // self
        assert!(RemotePath::new("/a/b/c").is_within(&base)); // descendant
        assert!(RemotePath::new("/a/b/c/d").is_within(&base)); // deep descendant
        assert!(!RemotePath::new("/a").is_within(&base)); // ancestor
        assert!(!RemotePath::new("/a/bc").is_within(&base)); // sibling prefix
        assert!(!RemotePath::new("/x").is_within(&base)); // unrelated
                                                          // The root contains everything.
        assert!(RemotePath::new("/a/b").is_within(&RemotePath::root()));
    }

    #[test]
    fn idempotent() {
        for s in ["/", "/a/b", "/a/./b/../c", "home//user/", ""] {
            let once = RemotePath::new(s);
            let twice = RemotePath::new(once.as_str());
            assert_eq!(once, twice);
        }
    }

    #[test]
    fn serde_round_trip() {
        let p = RemotePath::new("/var/www");
        let json = serde_json::to_string(&p).unwrap();
        let back: RemotePath = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    /// Navigation helpers reduce to `join`/`parent`/`components`: a breadcrumb at
    /// depth N rebuilt from the first N components yields the same prefix, and
    /// `open_dir` (join) then `go_up` (parent) returns to the original.
    #[test]
    fn navigation_round_trips() {
        let cwd = RemotePath::new("/var/www/html");

        // Breadcrumb rebuild: the prefix of the first N components.
        let rebuilt = |n: usize| {
            let mut path = RemotePath::root();
            for seg in cwd.components().take(n) {
                path = path.join(seg);
            }
            path
        };
        assert_eq!(rebuilt(0), RemotePath::root());
        assert_eq!(rebuilt(1).as_str(), "/var");
        assert_eq!(rebuilt(2).as_str(), "/var/www");
        assert_eq!(rebuilt(3), cwd);

        // open_dir then go_up.
        let down = cwd.join("assets");
        assert_eq!(down.as_str(), "/var/www/html/assets");
        assert_eq!(down.parent().unwrap(), cwd);
    }
}
