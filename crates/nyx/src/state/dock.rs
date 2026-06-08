//! Transfer-dock rows, counts, and clearing finished transfers.

use super::*;

impl AppState {
    /// The transfers visible under the active dock tab.
    pub fn dock_rows(&self) -> Vec<&TransferVm> {
        self.transfers
            .iter()
            .filter(|t| self.dock_tab.matches(t.transfer.status))
            .collect()
    }

    /// `(all, active, completed, failed)` dock counts.
    pub fn dock_counts(&self) -> (usize, usize, usize, usize) {
        let mut counts = (self.transfers.len(), 0, 0, 0);
        for t in &self.transfers {
            match t.transfer.status {
                TransferStatus::Running
                | TransferStatus::Queued
                | TransferStatus::AwaitingDecision
                | TransferStatus::Interrupted => counts.1 += 1,
                TransferStatus::Completed => counts.2 += 1,
                TransferStatus::Failed => counts.3 += 1,
                TransferStatus::Cancelled | TransferStatus::Skipped => {}
            }
        }
        counts
    }

    /// Clear finished (completed / failed / cancelled) transfers from the dock.
    pub fn clear_finished(&mut self) {
        self.transfers.retain(|t| {
            matches!(
                t.transfer.status,
                TransferStatus::Running
                    | TransferStatus::Queued
                    | TransferStatus::AwaitingDecision
                    | TransferStatus::Interrupted
            )
        });
    }
}
