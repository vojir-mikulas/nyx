//! Parses the browser's filter box into a query and matches entries against it.
//!
//! Bare words are case-insensitive substrings on the name (the historical
//! behavior); multiple bare words are AND-combined. A `*`/`?` switches a word to
//! a glob. `key:value` tokens add `type:`/`ext:`/`size:`/`modified:` predicates,
//! also AND-combined. A `"quoted phrase"` matches case-sensitively and keeps its
//! spaces. Anything that doesn't parse as a predicate falls back to a substring,
//! so a literal `foo:bar` filename still filters as typed.
//!
//! Matching is pure and allocation-light (it reuses each row's `name_lower`); it
//! runs in `rebuild_view_order`, never per frame.

use std::time::{Duration, SystemTime};

use nyx_core::EntryKind;

use super::models::EntryRow;

/// A parsed filter query: a set of AND-combined terms. The default (no terms)
/// matches everything, i.e. an empty filter box.
#[derive(Debug, Default, PartialEq)]
pub struct Filter {
    terms: Vec<Term>,
}

impl Filter {
    /// Parse the raw filter-box text into a query.
    pub fn parse(input: &str) -> Self {
        let terms = tokenize(input)
            .into_iter()
            .filter(|t| !t.text.is_empty())
            .map(|t| {
                if t.quoted {
                    Term::SubstrExact(t.text)
                } else {
                    parse_predicate(&t.text).unwrap_or_else(|| name_term(&t.text))
                }
            })
            .collect();
        Self { terms }
    }

    /// Whether `row` satisfies every term. `now` anchors relative `modified:`
    /// ages; pass a single snapshot for a whole rebuild.
    pub fn matches(&self, row: &EntryRow, now: SystemTime) -> bool {
        self.terms.iter().all(|t| t.matches(row, now))
    }
}

#[derive(Debug, PartialEq)]
enum Term {
    /// Case-insensitive substring on the name (compared against `name_lower`).
    Substr(String),
    /// Case-sensitive substring (a quoted term).
    SubstrExact(String),
    /// `*`/`?` glob on the name, matched case-insensitively (lowercased pattern).
    Glob(Vec<char>),
    /// Entry kind. `File` also accepts `Other` (sockets, fifos, …).
    Kind(EntryKind),
    /// File extension, lowercased, without the dot.
    Ext(String),
    /// Size comparison in bytes.
    Size(Cmp, u64),
    /// Modified within (`newer`) or before (`!newer`) a relative age.
    Age { newer: bool, dur: Duration },
}

impl Term {
    fn matches(&self, row: &EntryRow, now: SystemTime) -> bool {
        match self {
            Term::Substr(needle) => row.name_lower.contains(needle.as_str()),
            Term::SubstrExact(needle) => row.entry.name.contains(needle.as_str()),
            Term::Glob(pat) => {
                let name: Vec<char> = row.name_lower.chars().collect();
                glob_match(pat, &name)
            }
            Term::Kind(kind) => {
                row.entry.kind == *kind
                    || (*kind == EntryKind::File && row.entry.kind == EntryKind::Other)
            }
            Term::Ext(ext) => ext_of(&row.entry.name).is_some_and(|e| e == *ext),
            Term::Size(cmp, bytes) => cmp.test(row.entry.size, *bytes),
            Term::Age { newer, dur } => match (row.entry.modified, now.checked_sub(*dur)) {
                (Some(modified), Some(threshold)) => {
                    if *newer {
                        modified >= threshold
                    } else {
                        modified <= threshold
                    }
                }
                // No mtime can't satisfy an age bound; a duration past the epoch
                // means "everything is newer than that".
                (None, _) => false,
                (Some(_), None) => *newer,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

impl Cmp {
    fn test(self, a: u64, b: u64) -> bool {
        match self {
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
            Cmp::Gt => a > b,
            Cmp::Ge => a >= b,
            Cmp::Eq => a == b,
        }
    }
}

struct Token {
    text: String,
    quoted: bool,
}

/// Split on whitespace, but keep `"quoted phrases"` (with their spaces) as one
/// token. An unterminated quote runs to the end of input.
fn tokenize(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut text = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                text.push(c);
            }
            out.push(Token { text, quoted: true });
        } else {
            let mut text = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == '"' {
                    break;
                }
                text.push(c);
                chars.next();
            }
            out.push(Token {
                text,
                quoted: false,
            });
        }
    }
    out
}

/// A bare word: glob if it carries a wildcard, else a substring.
fn name_term(word: &str) -> Term {
    if word.contains('*') || word.contains('?') {
        Term::Glob(word.to_lowercase().chars().collect())
    } else {
        Term::Substr(word.to_lowercase())
    }
}

/// Parse a `key:value` predicate, or `None` if it isn't one (caller falls back
/// to a substring on the whole token).
fn parse_predicate(token: &str) -> Option<Term> {
    let (key, value) = token.split_once(':')?;
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    match key.to_ascii_lowercase().as_str() {
        "type" | "kind" => parse_kind(value),
        "ext" => Some(Term::Ext(value.trim_start_matches('.').to_lowercase())),
        "size" => parse_size(value),
        "modified" | "mtime" => parse_age(value),
        "name" => Some(name_term(value)),
        _ => None,
    }
}

fn parse_kind(value: &str) -> Option<Term> {
    let kind = match value.to_ascii_lowercase().as_str() {
        "dir" | "directory" | "folder" => EntryKind::Directory,
        "file" => EntryKind::File,
        "link" | "symlink" => EntryKind::Symlink,
        _ => return None,
    };
    Some(Term::Kind(kind))
}

fn parse_size(value: &str) -> Option<Term> {
    let (cmp, rest) = parse_cmp(value);
    let (num, unit) = split_number(rest.trim());
    let num: f64 = num.trim().parse().ok()?;
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        "g" | "gb" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    if num < 0.0 {
        return None;
    }
    Some(Term::Size(cmp.unwrap_or(Cmp::Eq), (num * mult) as u64))
}

fn parse_age(value: &str) -> Option<Term> {
    let (cmp, rest) = parse_cmp(value);
    let (num, unit) = split_number(rest.trim());
    let num: u64 = num.trim().parse().ok()?;
    let secs = match unit.trim().to_ascii_lowercase().as_str() {
        "s" => 1,
        "m" | "min" => 60,
        "h" => 3600,
        "" | "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    // `<N` means "within the last N" (newer); `>N` means "older than N".
    let newer = !matches!(cmp, Some(Cmp::Gt) | Some(Cmp::Ge));
    Some(Term::Age {
        newer,
        dur: Duration::from_secs(num.saturating_mul(secs)),
    })
}

/// Strip a leading comparison operator, returning it and the remainder.
fn parse_cmp(value: &str) -> (Option<Cmp>, &str) {
    if let Some(rest) = value.strip_prefix(">=") {
        (Some(Cmp::Ge), rest)
    } else if let Some(rest) = value.strip_prefix("<=") {
        (Some(Cmp::Le), rest)
    } else if let Some(rest) = value.strip_prefix('>') {
        (Some(Cmp::Gt), rest)
    } else if let Some(rest) = value.strip_prefix('<') {
        (Some(Cmp::Lt), rest)
    } else if let Some(rest) = value.strip_prefix('=') {
        (Some(Cmp::Eq), rest)
    } else {
        (None, value)
    }
}

/// Split a numeric prefix from a trailing unit, e.g. `"1.5m"` → `("1.5", "m")`.
fn split_number(s: &str) -> (&str, &str) {
    let at = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    s.split_at(at)
}

/// The lowercased extension of a name, or `None` for dotfiles / no dot.
fn ext_of(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext.to_lowercase())
}

/// Classic `*`/`?` glob with linear-time backtracking. `?` matches one char,
/// `*` matches any run (including empty); no character classes.
fn glob_match(pat: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut resume) = (None, 0usize);
    while t < text.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            resume = t;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_core::{Permissions, RemoteEntry};

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn row(name: &str, kind: EntryKind, size: u64, age: Option<Duration>) -> EntryRow {
        EntryRow::new(RemoteEntry {
            name: name.into(),
            size,
            kind,
            modified: age.map(|a| now() - a),
            permissions: Permissions::from_mode(0o644),
        })
    }

    fn file(name: &str) -> EntryRow {
        row(name, EntryKind::File, 0, None)
    }

    fn matches(query: &str, row: &EntryRow) -> bool {
        Filter::parse(query).matches(row, now())
    }

    #[test]
    fn empty_matches_everything() {
        assert!(matches("", &file("anything.txt")));
        assert!(matches("   ", &file("anything.txt")));
    }

    #[test]
    fn substring_is_case_insensitive() {
        assert!(matches("report", &file("Annual_REPORT.pdf")));
        assert!(!matches("report", &file("budget.pdf")));
    }

    #[test]
    fn multiple_words_are_anded() {
        assert!(matches("annual report", &file("annual_report_2024.pdf")));
        assert!(matches("report annual", &file("annual_report_2024.pdf")));
        assert!(!matches("annual report", &file("annual_budget.pdf")));
    }

    #[test]
    fn glob_anchors_full_name() {
        assert!(matches("*.rs", &file("main.rs")));
        assert!(!matches("*.rs", &file("main.rss")));
        assert!(matches("log_*", &file("log_2024.txt")));
        assert!(matches("?at.txt", &file("cat.txt")));
        assert!(!matches("?at.txt", &file("at.txt")));
    }

    #[test]
    fn type_predicate() {
        let dir = row("src", EntryKind::Directory, 0, None);
        assert!(matches("type:dir", &dir));
        assert!(matches("type:folder", &dir));
        assert!(!matches("type:file", &dir));
        assert!(matches("type:file", &file("a.txt")));
        assert!(matches(
            "type:file",
            &row("sock", EntryKind::Other, 0, None)
        ));
    }

    #[test]
    fn ext_predicate_and_dotfiles() {
        assert!(matches("ext:png", &file("logo.PNG")));
        assert!(matches("ext:.png", &file("logo.png")));
        assert!(!matches("ext:png", &file("logo.jpg")));
        assert!(!matches("ext:bashrc", &file(".bashrc")));
    }

    #[test]
    fn size_predicate_with_units() {
        let big = file_sized("big.bin", 5 * 1024 * 1024);
        assert!(matches("size:>1m", &big));
        assert!(matches("size:>=5mb", &big));
        assert!(!matches("size:<1m", &big));
        assert!(matches("size:<100k", &file_sized("tiny", 1000)));
    }

    fn file_sized(name: &str, size: u64) -> EntryRow {
        row(name, EntryKind::File, size, None)
    }

    #[test]
    fn modified_age() {
        let recent = file_aged("recent", Duration::from_secs(86_400)); // 1 day
        let old = file_aged("old", Duration::from_secs(30 * 86_400)); // 30 days
        assert!(matches("modified:<7d", &recent));
        assert!(!matches("modified:<7d", &old));
        assert!(matches("modified:>7d", &old));
        assert!(!matches("modified:>7d", &recent));
        assert!(!matches("modified:<7d", &file("no-mtime")));
    }

    fn file_aged(name: &str, age: Duration) -> EntryRow {
        row(name, EntryKind::File, 0, Some(age))
    }

    #[test]
    fn quoted_is_case_sensitive_and_keeps_spaces() {
        assert!(matches("\"My File\"", &file("My File.txt")));
        assert!(!matches("\"My File\"", &file("my file.txt")));
        assert!(!matches("\"My File\"", &file("MyFile.txt")));
    }

    #[test]
    fn combined_predicates_and_name() {
        let row = file_sized("report_final.pdf", 2 * 1024 * 1024);
        assert!(matches("report ext:pdf size:>1m", &row));
        assert!(!matches("report ext:pdf size:>5m", &row));
    }

    #[test]
    fn unparseable_predicate_falls_back_to_substring() {
        assert!(matches("foo:bar", &file("config.foo:bar")));
        assert!(matches("size:abc", &file("my-size:abc-file")));
    }
}
