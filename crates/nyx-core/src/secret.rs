//! The one wrapper every credential travels in.
//!
//! Passwords, SSH key passphrases and (later) host keys are carried as [`Secret`]
//! so the surrounding code - `Command`'s derived `Debug`, a stray `format!`, a
//! `tracing` field - can only ever see `***`. The plaintext is reachable solely
//! through [`Secret::expose`], the single audited escape hatch, called at the
//! auth boundary and nowhere else.

use std::fmt;

use zeroize::ZeroizeOnDrop;

/// A secret value that never reveals itself in `Debug`, `Display` or logs.
///
/// The inner string is reachable only via [`Secret::expose`]. There is
/// deliberately no `AsRef<str>`, `Deref` or `Into<String>` - those would make
/// leaking the value ergonomic. The buffer is zeroed on drop as
/// defense-in-depth (redaction is the guarantee; zeroization is the backstop).
#[derive(Clone, ZeroizeOnDrop)]
pub struct Secret(String);

impl Secret {
    /// Wrap a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reveal the secret. Call sites must not log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "***");
        assert!(!format!("{s:?}").contains("hunter2"));
    }

    #[test]
    fn display_redacts() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s}"), "***");
        assert!(!format!("{s}").contains("hunter2"));
    }

    #[test]
    fn expose_returns_the_value() {
        let s = Secret::new("hunter2");
        assert_eq!(s.expose(), "hunter2");
    }
}
