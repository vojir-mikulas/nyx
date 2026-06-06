//! Remote-filesystem domain types: protocols and directory entries.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// A remote-access protocol.
///
/// V1 ships SFTP only; FTP/FTPS are modelled here so the `RemoteClient`
/// abstraction (in `nyx-protocol`) can grow without touching shared types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// SSH File Transfer Protocol.
    Sftp,
    /// File Transfer Protocol (plain).
    Ftp,
    /// FTP over TLS.
    Ftps,
}

impl Protocol {
    /// The conventional default TCP port for this protocol.
    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Sftp => 22,
            Protocol::Ftp | Protocol::Ftps => 21,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ports() {
        assert_eq!(Protocol::Sftp.default_port(), 22);
        assert_eq!(Protocol::Ftp.default_port(), 21);
        assert_eq!(Protocol::Ftps.default_port(), 21);
    }

    #[test]
    fn rwx_string_renders_the_triad() {
        assert_eq!(Permissions::from_mode(0o755).rwx_string(), "rwxr-xr-x");
        assert_eq!(Permissions::from_mode(0o644).rwx_string(), "rw-r--r--");
        assert_eq!(Permissions::from_mode(0o000).rwx_string(), "---------");
        assert_eq!(Permissions::from_mode(0o777).rwx_string(), "rwxrwxrwx");
        // setuid/setgid/sticky land in the execute slot (lowercase when the
        // execute bit is also set).
        assert_eq!(Permissions::from_mode(0o4755).rwx_string(), "rwsr-xr-x");
        assert_eq!(Permissions::from_mode(0o2755).rwx_string(), "rwxr-sr-x");
        assert_eq!(Permissions::from_mode(0o1755).rwx_string(), "rwxr-xr-t");
        // Uppercase when the special bit is set but the execute bit is not.
        assert_eq!(Permissions::from_mode(0o4644).rwx_string(), "rwSr--r--");
    }

    #[test]
    fn triads_decode_the_right_bits() {
        let p = Permissions::from_mode(0o751);
        assert_eq!(
            p.user(),
            Rwx {
                read: true,
                write: true,
                execute: true
            }
        );
        assert_eq!(
            p.group(),
            Rwx {
                read: true,
                write: false,
                execute: true
            }
        );
        assert_eq!(
            p.other(),
            Rwx {
                read: false,
                write: false,
                execute: true
            }
        );
    }

    #[test]
    fn perm_bits_masks_off_type_and_special_bits() {
        // Type bits (0o100_000 = regular file) and setuid above the low 9 drop.
        assert_eq!(Permissions::from_mode(0o104_755).perm_bits(), 0o755);
        assert_eq!(Permissions::from_mode(0o104_755).mode(), 0o104_755);
    }

    #[test]
    fn is_dir_agrees_with_kind() {
        let dir = RemoteEntry {
            name: "etc".into(),
            size: 0,
            kind: EntryKind::Directory,
            modified: None,
            permissions: Permissions::from_mode(0o755),
        };
        let file = RemoteEntry {
            kind: EntryKind::File,
            ..dir.clone()
        };
        assert!(dir.is_dir());
        assert!(!file.is_dir());
    }

    #[test]
    fn permissions_serde_round_trip() {
        let p = Permissions::from_mode(0o4755);
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(
            Permissions::from_mode(0o4755),
            serde_json::from_str(&json).unwrap()
        );

        let entry = RemoteEntry {
            name: "report.pdf".into(),
            size: 1234,
            kind: EntryKind::File,
            modified: None,
            permissions: p,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(entry, serde_json::from_str(&json).unwrap());
    }
}

/// The kind of a directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
    /// Anything else (socket, device, fifo, …).
    Other,
}

/// A read/write/execute triad (one of user/group/other).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rwx {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

/// A file's Unix permission bits, kept as the raw mode reported by the server.
///
/// The classic `rwxr-xr-x` string is a *rendered view* ([`rwx_string`]), not
/// stored data — this keeps the real mode available for sorting and a future
/// chmod.
///
/// [`rwx_string`]: Permissions::rwx_string
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    /// Raw Unix mode bits as reported by the server (e.g. `0o755`).
    mode: u32,
}

impl Permissions {
    pub fn from_mode(mode: u32) -> Self {
        Self { mode }
    }

    /// The full raw mode, including file-type and setuid/setgid/sticky bits.
    pub fn mode(&self) -> u32 {
        self.mode
    }

    /// Permission bits only (mask `0o777`), without the type/special bits.
    pub fn perm_bits(&self) -> u32 {
        self.mode & 0o777
    }

    pub fn user(&self) -> Rwx {
        Rwx::from_bits((self.mode >> 6) & 0o7)
    }

    pub fn group(&self) -> Rwx {
        Rwx::from_bits((self.mode >> 3) & 0o7)
    }

    pub fn other(&self) -> Rwx {
        Rwx::from_bits(self.mode & 0o7)
    }

    pub fn setuid(&self) -> bool {
        self.mode & 0o4000 != 0
    }

    pub fn setgid(&self) -> bool {
        self.mode & 0o2000 != 0
    }

    pub fn sticky(&self) -> bool {
        self.mode & 0o1000 != 0
    }

    /// Render the classic `rwxr-xr-x` triad (9 chars, no type prefix).
    ///
    /// setuid/setgid/sticky bits show in the execute slot as `s`/`s`/`t` (or
    /// uppercase `S`/`T` when the matching execute bit is clear), as in `ls`.
    pub fn rwx_string(&self) -> String {
        let mut out = String::with_capacity(9);
        triad(self.user(), self.setuid(), 's', &mut out);
        triad(self.group(), self.setgid(), 's', &mut out);
        triad(self.other(), self.sticky(), 't', &mut out);
        out
    }
}

impl Rwx {
    fn from_bits(bits: u32) -> Self {
        Self {
            read: bits & 0o4 != 0,
            write: bits & 0o2 != 0,
            execute: bits & 0o1 != 0,
        }
    }
}

/// Push one `rwx` triad onto `out`, folding in a special bit (setuid/setgid/
/// sticky) at the execute slot the way `ls -l` renders it.
fn triad(rwx: Rwx, special: bool, special_ch: char, out: &mut String) {
    out.push(if rwx.read { 'r' } else { '-' });
    out.push(if rwx.write { 'w' } else { '-' });
    out.push(match (special, rwx.execute) {
        (true, true) => special_ch,
        (true, false) => special_ch.to_ascii_uppercase(),
        (false, true) => 'x',
        (false, false) => '-',
    });
}

/// A single entry in a remote directory listing.
///
/// This is the unit the file browser renders. The UI maps it to its own row
/// props — `nyx-ui` components never see this type directly (see plan-02).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteEntry {
    /// File or directory name (the final path component).
    pub name: String,
    /// Size in bytes. Meaningless for directories; conventionally `0`.
    pub size: u64,
    /// The entry kind.
    pub kind: EntryKind,
    /// Last-modified time, if the server reported one.
    pub modified: Option<SystemTime>,
    /// Unix permission bits as reported by the server.
    pub permissions: Permissions,
}

impl RemoteEntry {
    /// Whether this entry is a directory.
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Directory)
    }
}
