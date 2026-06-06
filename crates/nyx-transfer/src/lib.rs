// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! Transfer queue for Nyx.
//!
//! Owns the set of queued / running transfers, the concurrency policy, progress
//! accounting and cancellation. Stub only — the scheduling logic lands in a
//! later plan. The types here establish the shape the service drives.

use nyx_core::{Transfer, TransferId};

/// An in-memory queue of transfers.
///
/// The real implementation will track concurrency limits, drive
/// [`nyx_protocol::RemoteClient`] transfers and emit progress. For now it only
/// assigns ids and holds records.
#[derive(Default)]
pub struct TransferQueue {
    next_id: u64,
    transfers: Vec<Transfer>,
}

impl TransferQueue {
    /// Create an empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the next unique [`TransferId`].
    pub fn next_id(&mut self) -> TransferId {
        let id = TransferId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Add a transfer to the queue, returning its id.
    pub fn enqueue(&mut self, transfer: Transfer) -> TransferId {
        let id = transfer.id;
        self.transfers.push(transfer);
        id
    }

    /// All transfers currently known to the queue.
    pub fn transfers(&self) -> &[Transfer] {
        &self.transfers
    }
}
