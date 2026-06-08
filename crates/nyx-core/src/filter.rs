//! Parses the browser's filter box into a query and matches entries against it.
//!
//! A leading `/` sets the [`Scope`] to a recursive tree search; otherwise the
//! query filters the current directory. After the optional sigil: bare words are
//! case-insensitive substrings on the name (AND-combined); `*`/`?` switch a word
//! to a glob; `key:value` tokens add `type:`/`ext:`/`size:`/`modified:`
//! predicates; a `"quoted phrase"` matches case-sensitively and keeps its spaces.
//! Anything that doesn't parse as a predicate falls back to a substring, so a
//! literal `foo:bar` filename still filters as typed.
//!
//! Matching is pure and allocation-light — the caller passes a precomputed
//! lowercased name (the browser caches one per row; the tree search lowercases on
//! the fly), so a substring/sort never re-lowercases.

use std::time::{Duration, SystemTime};

use crate::remote::{EntryKind, RemoteEntry};

/// One `find` predicate for a server-side search. The protocol layer renders
/// these into a (shell-quoted) `find` command; this stays pure data so `nyx-core`
/// keeps no shell/runtime knowledge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindPredicate {
    /// Case-insensitive name glob → `-iname '<glob>'`.
    Iname(String),
    /// Case-sensitive name glob (from a quoted term) → `-name '<glob>'`.
    Name(String),
    /// Restrict to a kind → `-type d|f|l`.
    Kind(EntryKind),
}

/// Escape `find`-glob metacharacters so a literal substring matches literally.
fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Where a query searches: the current directory (default) or the whole subtree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
    /// Filter the entries already listed for the current directory.
    #[default]
    CurrentDir,
    /// Recursively search the subtree, triggered by a leading `/`.
    Tree,
}

/// A parsed filter query: a [`Scope`] plus AND-combined terms. The default (no
/// terms) matches everything, i.e. an empty filter box.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    scope: Scope,
    terms: Vec<Term>,
}

impl Filter {
    /// Parse the raw filter-box text into a query.
    pub fn parse(input: &str) -> Self {
        let trimmed = input.trim_start();
        let (scope, rest) = match trimmed.strip_prefix('/') {
            Some(rest) => (Scope::Tree, rest),
            None => (Scope::CurrentDir, trimmed),
        };
        let terms = tokenize(rest)
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
        Self { scope, terms }
    }

    /// The query's scope (current directory vs. recursive tree search).
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// Whether the query has no terms (an empty or sigil-only box).
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether `entry` satisfies every term. `name_lower` must be the entry's
    /// lowercased name; `now` anchors relative `modified:` ages — pass one
    /// snapshot for a whole pass.
    pub fn matches(&self, entry: &RemoteEntry, name_lower: &str, now: SystemTime) -> bool {
        self.terms.iter().all(|t| t.matches(entry, name_lower, now))
    }

    /// Translate this query into `find` predicates for a server-side search, or
    /// `None` when it can't be fully expressed (a `size:`/`modified:` term, or no
    /// terms) so the caller falls back to a client-side walk. Name/glob/`ext:`/
    /// `type:` map cleanly to portable `-iname`/`-name`/`-type` predicates,
    /// AND-combined (find's default).
    pub fn as_find_predicates(&self) -> Option<Vec<FindPredicate>> {
        if self.terms.is_empty() {
            return None;
        }
        let mut preds = Vec::with_capacity(self.terms.len());
        for term in &self.terms {
            preds.push(match term {
                Term::Substr(s) => FindPredicate::Iname(format!("*{}*", glob_escape(s))),
                Term::SubstrExact(s) => FindPredicate::Name(format!("*{}*", glob_escape(s))),
                Term::Glob(chars) => FindPredicate::Iname(chars.iter().collect()),
                Term::Ext(e) => FindPredicate::Iname(format!("*.{}", glob_escape(e))),
                Term::Kind(k) => FindPredicate::Kind(*k),
                // `find` can express these, but units/semantics diverge from our
                // matcher — defer to the client walk for exact fidelity.
                Term::Size(..) | Term::Age { .. } => return None,
            });
        }
        Some(preds)
    }
}

#[derive(Debug, Clone, PartialEq)]
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
    fn matches(&self, entry: &RemoteEntry, name_lower: &str, now: SystemTime) -> bool {
        match self {
            Term::Substr(needle) => name_lower.contains(needle.as_str()),
            Term::SubstrExact(needle) => entry.name.contains(needle.as_str()),
            Term::Glob(pat) => {
                let name: Vec<char> = name_lower.chars().collect();
                glob_match(pat, &name)
            }
            Term::Kind(kind) => {
                entry.kind == *kind || (*kind == EntryKind::File && entry.kind == EntryKind::Other)
            }
            Term::Ext(ext) => ext_of(&entry.name).is_some_and(|e| e == *ext),
            Term::Size(cmp, bytes) => cmp.test(entry.size, *bytes),
            Term::Age { newer, dur } => match (entry.modified, now.checked_sub(*dur)) {
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
    use crate::Permissions;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn entry(name: &str, kind: EntryKind, size: u64, age: Option<Duration>) -> RemoteEntry {
        RemoteEntry {
            name: name.into(),
            size,
            kind,
            modified: age.map(|a| now() - a),
            permissions: Permissions::from_mode(0o644),
        }
    }

    fn file(name: &str) -> RemoteEntry {
        entry(name, EntryKind::File, 0, None)
    }

    fn matches(query: &str, e: &RemoteEntry) -> bool {
        Filter::parse(query).matches(e, &e.name.to_lowercase(), now())
    }

    #[test]
    fn empty_matches_everything() {
        assert!(matches("", &file("anything.txt")));
        assert!(matches("   ", &file("anything.txt")));
    }

    #[test]
    fn scope_sigil_is_parsed_and_stripped() {
        assert_eq!(Filter::parse("report").scope(), Scope::CurrentDir);
        assert_eq!(Filter::parse("/report").scope(), Scope::Tree);
        assert_eq!(Filter::parse("  /*.rs").scope(), Scope::Tree);
        // The sigil doesn't leak into matching.
        assert!(matches("/report", &file("report.txt")));
        assert!(matches("/*.rs", &file("main.rs")));
        assert!(Filter::parse("/").is_empty());
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
        let dir = entry("src", EntryKind::Directory, 0, None);
        assert!(matches("type:dir", &dir));
        assert!(matches("type:folder", &dir));
        assert!(!matches("type:file", &dir));
        assert!(matches("type:file", &file("a.txt")));
        assert!(matches(
            "type:file",
            &entry("sock", EntryKind::Other, 0, None)
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
        let big = entry("big.bin", EntryKind::File, 5 * 1024 * 1024, None);
        assert!(matches("size:>1m", &big));
        assert!(matches("size:>=5mb", &big));
        assert!(!matches("size:<1m", &big));
        assert!(matches(
            "size:<100k",
            &entry("tiny", EntryKind::File, 1000, None)
        ));
    }

    #[test]
    fn modified_age() {
        let recent = entry(
            "recent",
            EntryKind::File,
            0,
            Some(Duration::from_secs(86_400)),
        );
        let old = entry(
            "old",
            EntryKind::File,
            0,
            Some(Duration::from_secs(30 * 86_400)),
        );
        assert!(matches("modified:<7d", &recent));
        assert!(!matches("modified:<7d", &old));
        assert!(matches("modified:>7d", &old));
        assert!(!matches("modified:>7d", &recent));
        assert!(!matches("modified:<7d", &file("no-mtime")));
    }

    #[test]
    fn quoted_is_case_sensitive_and_keeps_spaces() {
        assert!(matches("\"My File\"", &file("My File.txt")));
        assert!(!matches("\"My File\"", &file("my file.txt")));
        assert!(!matches("\"My File\"", &file("MyFile.txt")));
    }

    #[test]
    fn combined_predicates_and_name() {
        let row = entry("report_final.pdf", EntryKind::File, 2 * 1024 * 1024, None);
        assert!(matches("report ext:pdf size:>1m", &row));
        assert!(!matches("report ext:pdf size:>5m", &row));
    }

    #[test]
    fn unparseable_predicate_falls_back_to_substring() {
        assert!(matches("foo:bar", &file("config.foo:bar")));
        assert!(matches("size:abc", &file("my-size:abc-file")));
    }

    #[test]
    fn find_predicates_translate_expressible_queries() {
        use FindPredicate::{Iname, Kind, Name};
        assert_eq!(
            Filter::parse("/*.rs").as_find_predicates(),
            Some(vec![Iname("*.rs".into())])
        );
        assert_eq!(
            Filter::parse("report").as_find_predicates(),
            Some(vec![Iname("*report*".into())])
        );
        assert_eq!(
            Filter::parse("ext:png type:file").as_find_predicates(),
            Some(vec![Iname("*.png".into()), Kind(EntryKind::File)])
        );
        assert_eq!(
            Filter::parse("\"Foo\"").as_find_predicates(),
            Some(vec![Name("*Foo*".into())])
        );
        // A bare `*` is a glob (passed through); a quoted one is a literal, escaped.
        assert_eq!(
            Filter::parse("a*b").as_find_predicates(),
            Some(vec![Iname("a*b".into())])
        );
        assert_eq!(
            Filter::parse("\"a*b\"").as_find_predicates(),
            Some(vec![Name("*a\\*b*".into())])
        );
    }

    #[test]
    fn find_predicates_bail_when_not_expressible() {
        assert_eq!(Filter::parse("report size:>1m").as_find_predicates(), None);
        assert_eq!(Filter::parse("modified:<7d").as_find_predicates(), None);
        assert_eq!(Filter::parse("").as_find_predicates(), None);
        assert_eq!(Filter::parse("/").as_find_predicates(), None);
    }
}
