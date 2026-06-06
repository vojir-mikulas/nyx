// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! In-memory fake data for M1.
//!
//! These stand in for the backend until M2 wires real `DirListing` /
//! `TransferProgress` events. The shapes match what the service will emit, so
//! the derived getters (`visible_entries`, `dock_rows`) need no change later —
//! only the *source* swaps.

use gpui::SharedString;
use nyx_core::{
    EntryKind, Protocol, RemoteEntry, Transfer, TransferDirection, TransferId, TransferStatus,
};
use nyx_profile::Profile;

use super::models::{ymd_hm, AccentKind, ConnectionVm, EntryRow, TransferVm};

/// The saved + recent connection profiles shown in the sidebar and welcome.
pub fn fake_connections() -> Vec<ConnectionVm> {
    fn profile(
        id: &str,
        name: &str,
        protocol: Protocol,
        host: &str,
        port: u16,
        username: &str,
        path: &str,
    ) -> Profile {
        Profile {
            id: id.to_string(),
            name: name.to_string(),
            protocol,
            host: host.to_string(),
            port,
            username: username.to_string(),
            remote_path: Some(path.to_string()),
        }
    }

    vec![
        ConnectionVm {
            profile: profile(
                "prod",
                "prod-web-01",
                Protocol::Sftp,
                "188.34.201.44",
                22,
                "deploy",
                "/var/www/nyx-app/current",
            ),
            color: AccentKind::Purple,
            last_used: Some("4m ago".into()),
            is_recent: true,
        },
        ConnectionVm {
            profile: profile(
                "staging",
                "staging",
                Protocol::Sftp,
                "staging.nyx.dev",
                22,
                "deploy",
                "/var/www",
            ),
            color: AccentKind::Purple,
            last_used: Some("2h ago".into()),
            is_recent: true,
        },
        ConnectionVm {
            profile: profile(
                "cdn",
                "media-cdn",
                Protocol::Ftps,
                "cdn.nyx.dev",
                990,
                "ftp_cdn",
                "/assets",
            ),
            color: AccentKind::Green,
            last_used: Some("1d ago".into()),
            is_recent: true,
        },
        ConnectionVm {
            profile: profile(
                "backup",
                "legacy-backup",
                Protocol::Ftp,
                "backup.internal.net",
                21,
                "backup",
                "/daily",
            ),
            color: AccentKind::Blue,
            last_used: Some("5d ago".into()),
            is_recent: false,
        },
    ]
}

/// Build a file entry.
fn file(name: &str, size: u64, perms: &str, modified: (i64, u32, u32, u32, u32)) -> RemoteEntry {
    let (y, mo, d, h, mi) = modified;
    RemoteEntry {
        name: name.to_string(),
        size,
        kind: EntryKind::File,
        modified: Some(ymd_hm(y, mo, d, h, mi)),
        perms: perms.to_string(),
        is_dir: false,
    }
}

/// Build a directory entry.
fn dir(name: &str, modified: (i64, u32, u32, u32, u32)) -> RemoteEntry {
    let (y, mo, d, h, mi) = modified;
    RemoteEntry {
        name: name.to_string(),
        size: 0,
        kind: EntryKind::Directory,
        modified: Some(ymd_hm(y, mo, d, h, mi)),
        perms: "rwxr-xr-x".to_string(),
        is_dir: true,
    }
}

/// A canned listing for the current working directory.
///
/// Known paths return believable contents; unknown paths return empty (so deep
/// navigation gracefully shows the empty state). M2 replaces this with real
/// `DirListing` events keyed by the same path segments.
pub fn fake_listing(cwd: &[SharedString]) -> Vec<EntryRow> {
    let path: Vec<&str> = cwd.iter().map(|s| s.as_ref()).collect();
    let entries = match path.as_slice() {
        [] => vec![
            dir("var", (2026, 5, 12, 9, 14)),
            dir("home", (2026, 5, 2, 12, 0)),
            dir("etc", (2026, 4, 18, 8, 0)),
            dir("opt", (2026, 3, 1, 10, 30)),
            dir("srv", (2026, 5, 28, 14, 2)),
            file("deploy.sh", 2204, "rwxr-xr-x", (2026, 5, 28, 11, 0)),
            file("docker-compose.yml", 1842, "rw-r--r--", (2026, 6, 1, 8, 31)),
            file("nginx.conf", 2841, "rw-r--r--", (2026, 5, 28, 11, 10)),
            file(".env.production", 488, "rw-------", (2026, 6, 1, 8, 31)),
            file("README.md", 4821, "rw-r--r--", (2026, 6, 2, 12, 1)),
            file(
                "backup.tar.gz",
                482_220_114,
                "rw-r--r--",
                (2026, 6, 4, 3, 0),
            ),
            file("access.log", 188_223_101, "rw-r--r--", (2026, 6, 5, 18, 0)),
            file("favicon.ico", 15022, "rw-r--r--", (2026, 1, 2, 0, 0)),
            file("hero-2026.webp", 442_112, "rw-r--r--", (2026, 6, 5, 16, 20)),
            file(
                "demo-loop.mp4",
                28_442_119,
                "rw-r--r--",
                (2026, 5, 30, 12, 0),
            ),
            file("export-may.csv", 5_520_011, "rw-r--r--", (2026, 6, 1, 3, 0)),
            file("package.json", 1842, "rw-r--r--", (2026, 6, 4, 22, 8)),
            file("styles.7be2.css", 64120, "rw-r--r--", (2026, 6, 4, 22, 9)),
        ],
        ["var"] => vec![
            dir("www", (2026, 6, 4, 22, 10)),
            dir("log", (2026, 6, 5, 18, 0)),
            dir("lib", (2026, 4, 11, 9, 0)),
            dir("cache", (2026, 6, 5, 17, 30)),
        ],
        ["var", "www"] => vec![
            dir("nyx-app", (2026, 6, 4, 22, 10)),
            dir("html", (2026, 5, 20, 10, 0)),
            file("index.html", 4213, "rw-r--r--", (2026, 6, 4, 22, 9)),
            file("robots.txt", 112, "rw-r--r--", (2026, 5, 20, 10, 0)),
        ],
        ["var", "www", "nyx-app"] => vec![
            dir("current", (2026, 6, 4, 22, 9)),
            dir("releases", (2026, 6, 4, 22, 9)),
            dir("shared", (2026, 5, 31, 9, 33)),
        ],
        ["var", "www", "nyx-app", "current"] => vec![
            dir("dist", (2026, 6, 4, 22, 9)),
            file("package.json", 1842, "rw-r--r--", (2026, 6, 4, 22, 8)),
            file(
                "ecosystem.config.js",
                612,
                "rw-r--r--",
                (2026, 5, 28, 11, 2),
            ),
            file(".env.production", 488, "rw-------", (2026, 6, 1, 8, 31)),
        ],
        ["var", "www", "nyx-app", "current", "dist"] => vec![
            file("index.html", 4213, "rw-r--r--", (2026, 6, 4, 22, 9)),
            file("app.4f9c1a.js", 882_441, "rw-r--r--", (2026, 6, 4, 22, 9)),
            file(
                "app.4f9c1a.js.map",
                2_310_558,
                "rw-r--r--",
                (2026, 6, 4, 22, 9),
            ),
            file(
                "vendor.aa31.js",
                1_442_098,
                "rw-r--r--",
                (2026, 6, 4, 22, 9),
            ),
            file("styles.7be2.css", 64120, "rw-r--r--", (2026, 6, 4, 22, 9)),
        ],
        ["home"] => vec![
            dir("deploy", (2026, 6, 4, 22, 10)),
            dir("admin", (2026, 5, 2, 12, 0)),
        ],
        ["etc"] => vec![
            dir("nginx", (2026, 5, 28, 11, 10)),
            file("hosts", 412, "rw-r--r--", (2026, 4, 18, 8, 0)),
            file("fstab", 821, "rw-r--r--", (2026, 1, 14, 10, 0)),
        ],
        _ => vec![],
    };
    entries.into_iter().map(EntryRow::new).collect()
}

/// The seed transfers shown in the dock — a spread across every status and both
/// directions, with synthetic speed/error.
pub fn fake_transfers() -> Vec<TransferVm> {
    #[allow(clippy::too_many_arguments)] // a flat fixture builder reads clearest here
    fn vm(
        id: u64,
        direction: TransferDirection,
        remote: &str,
        local: &str,
        total: u64,
        done: u64,
        status: TransferStatus,
        speed: Option<u64>,
        error: Option<&str>,
    ) -> TransferVm {
        TransferVm {
            transfer: Transfer {
                id: TransferId(id),
                direction,
                remote_path: remote.to_string(),
                local_path: local.to_string(),
                total_bytes: Some(total),
                transferred_bytes: done,
                status,
            },
            speed_bps: speed,
            error: error.map(Into::into),
        }
    }

    let dist = "/var/www/nyx-app/current/dist";
    vec![
        vm(
            1,
            TransferDirection::Upload,
            &format!("{dist}/app.4f9c1a.js"),
            "~/work/dist/app.4f9c1a.js",
            882_441,
            547_113,
            TransferStatus::Running,
            Some(4_299_161),
            None,
        ),
        vm(
            2,
            TransferDirection::Upload,
            &format!("{dist}/vendor.aa31.js"),
            "~/work/dist/vendor.aa31.js",
            1_442_098,
            403_787,
            TransferStatus::Running,
            Some(3_879_001),
            None,
        ),
        vm(
            3,
            TransferDirection::Download,
            "/var/www/shared/uploads/export-may.csv",
            "~/Downloads/export-may.csv",
            5_520_011,
            4_636_809,
            TransferStatus::Running,
            Some(6_501_171),
            None,
        ),
        vm(
            4,
            TransferDirection::Upload,
            &format!("{dist}/styles.7be2.css"),
            "~/work/dist/styles.7be2.css",
            64_120,
            0,
            TransferStatus::Queued,
            None,
            None,
        ),
        vm(
            5,
            TransferDirection::Upload,
            &format!("{dist}/index.html"),
            "~/work/dist/index.html",
            4_213,
            4_213,
            TransferStatus::Completed,
            None,
            None,
        ),
        vm(
            6,
            TransferDirection::Download,
            "/home/deploy/backup.tar.gz",
            "~/Backups/backup.tar.gz",
            482_220_114,
            482_220_114,
            TransferStatus::Completed,
            None,
            None,
        ),
        vm(
            7,
            TransferDirection::Download,
            "/var/log/nginx/error.log",
            "~/Downloads/error.log",
            2_204_881,
            903_001,
            TransferStatus::Failed,
            None,
            Some("Connection reset by peer"),
        ),
        vm(
            8,
            TransferDirection::Upload,
            "/var/www/html/old-index.html",
            "~/work/old-index.html",
            12_882,
            5_120,
            TransferStatus::Cancelled,
            None,
            None,
        ),
    ]
}
