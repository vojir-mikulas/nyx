// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! In-memory fake data.
//!
//! M2 wired the file browser to real `DirListing` events and M3 swapped the
//! connection list for the real `FileProfileStore`, so only the transfer dock
//! stays synthetic — until the live transfer queue lands in M5. The shapes still
//! match what the service emits, so the derived getters need no change when the
//! source swaps.

use nyx_core::{Transfer, TransferDirection, TransferId, TransferStatus};

use super::models::TransferVm;

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
