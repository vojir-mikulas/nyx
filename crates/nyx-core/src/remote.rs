// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

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
    /// Permission string as presented to the user (e.g. `"rwxr-xr-x"`).
    pub perms: String,
    /// Convenience flag mirroring `kind == Directory` (or a dir symlink).
    pub is_dir: bool,
}
