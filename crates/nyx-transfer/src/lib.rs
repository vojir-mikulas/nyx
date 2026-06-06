// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! Transfer queue for Nyx.
//!
//! A **sans-IO scheduler**: pure policy with no tokio, no protocol and no
//! spawning. It owns the concurrency cap, the queue of pending specs, id
//! allocation and — per running transfer — a shared [`TransferProgress`] handle
//! (a byte counter + a one-way cancel flag). The service drives it: it `submit`s
//! specs, `poll_start`s to promote queued transfers into running slots (spawning
//! the actual copy tasks), `finish`es them on completion and `cancel`s on
//! request. Keeping the queue free of I/O makes the policy unit-testable without
//! a runtime or a server.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use nyx_core::{TransferDirection, TransferId, TransferProgress};

/// What the service hands in to enqueue a transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferSpec {
    /// Upload or download.
    pub direction: TransferDirection,
    /// The remote-side path.
    pub remote: String,
    /// The local-side path.
    pub local: PathBuf,
}

/// What [`TransferQueue::poll_start`] hands back when a queued transfer is
/// promoted into a running slot.
#[derive(Debug, Clone)]
pub struct Started {
    /// The promoted transfer's id.
    pub id: TransferId,
    /// Its spec (the service runs the copy from this).
    pub spec: TransferSpec,
    /// The shared progress + cancel handle (the service passes this into the
    /// protocol copy; the queue keeps a clone to read bytes and request cancel).
    pub progress: TransferProgress,
}

/// The result of a [`TransferQueue::cancel`] request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The id was still queued; it was dropped and will never start.
    WasQueued,
    /// The id was running; its cancel flag was set (the copy winds down).
    WasRunning,
    /// The id is unknown (already finished, or never existed).
    Unknown,
}

/// An in-memory scheduler: a concurrency cap, a FIFO queue of pending specs and
/// the set of running transfers' progress handles.
pub struct TransferQueue {
    /// The maximum number of transfers running at once.
    cap: usize,
    /// The next id to assign (monotonic).
    next_id: u64,
    /// Pending transfers, oldest first.
    queued: VecDeque<(TransferId, TransferSpec)>,
    /// Running transfers, by id, with their shared progress handles.
    running: HashMap<TransferId, TransferProgress>,
}

impl TransferQueue {
    /// Create an empty queue with the given concurrency cap.
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            next_id: 0,
            queued: VecDeque::new(),
            running: HashMap::new(),
        }
    }

    /// Assign an id and push `spec` onto the queue, returning the new id.
    pub fn submit(&mut self, spec: TransferSpec) -> TransferId {
        let id = TransferId(self.next_id);
        self.next_id += 1;
        self.queued.push_back((id, spec));
        id
    }

    /// If a slot is free and a transfer is queued, promote the oldest one into a
    /// running slot and return its [`Started`] handle; otherwise `None`.
    pub fn poll_start(&mut self) -> Option<Started> {
        if self.running.len() >= self.cap {
            return None;
        }
        let (id, spec) = self.queued.pop_front()?;
        let progress = TransferProgress::default();
        self.running.insert(id, progress.clone());
        Some(Started { id, spec, progress })
    }

    /// Drop a transfer from the running set (it reached a terminal state).
    pub fn finish(&mut self, id: TransferId) {
        self.running.remove(&id);
    }

    /// Cancel a transfer by id. A queued transfer is dropped immediately; a
    /// running one has its cancel flag set (the copy loop notices and winds
    /// down, then the service `finish`es it through the normal terminal path).
    pub fn cancel(&mut self, id: TransferId) -> CancelOutcome {
        if let Some(pos) = self.queued.iter().position(|(qid, _)| *qid == id) {
            self.queued.remove(pos);
            return CancelOutcome::WasQueued;
        }
        if let Some(progress) = self.running.get(&id) {
            progress.cancel();
            return CancelOutcome::WasRunning;
        }
        CancelOutcome::Unknown
    }

    /// Cancel everything: flag every running transfer and drain the queue.
    /// Returns the ids of the dropped queued transfers (the service emits a
    /// terminal `Cancelled` event for each, since no task ever ran for them).
    pub fn cancel_all(&mut self) -> Vec<TransferId> {
        for progress in self.running.values() {
            progress.cancel();
        }
        self.queued.drain(..).map(|(id, _)| id).collect()
    }

    /// Each running transfer's `(id, bytes transferred so far)`, for the
    /// service's progress ticker.
    pub fn running_progress(&self) -> impl Iterator<Item = (TransferId, u64)> + '_ {
        self.running.iter().map(|(id, p)| (*id, p.transferred()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(n: u64) -> TransferSpec {
        TransferSpec {
            direction: TransferDirection::Download,
            remote: format!("/r/{n}"),
            local: PathBuf::from(format!("/l/{n}")),
        }
    }

    #[test]
    fn cap_is_enforced_and_backfills_on_finish() {
        let mut q = TransferQueue::new(3);
        let ids: Vec<_> = (0..5).map(|n| q.submit(spec(n))).collect();

        // Exactly `cap` start, then the queue stalls.
        let started: Vec<_> = std::iter::from_fn(|| q.poll_start()).collect();
        assert_eq!(started.len(), 3);
        assert_eq!(
            started.iter().map(|s| s.id).collect::<Vec<_>>(),
            ids[..3].to_vec()
        );
        assert!(q.poll_start().is_none());

        // Finishing one frees a slot; the 4th (FIFO) starts next.
        q.finish(ids[0]);
        let fourth = q.poll_start().expect("a slot freed");
        assert_eq!(fourth.id, ids[3]);
        assert!(q.poll_start().is_none());
    }

    #[test]
    fn cancel_queued_drops_it_before_it_starts() {
        let mut q = TransferQueue::new(1);
        let a = q.submit(spec(0));
        let b = q.submit(spec(1));

        // `a` is running, `b` is still queued.
        assert_eq!(q.poll_start().unwrap().id, a);

        assert_eq!(q.cancel(b), CancelOutcome::WasQueued);
        // `b` never starts even once the slot frees.
        q.finish(a);
        assert!(q.poll_start().is_none());
    }

    #[test]
    fn cancel_running_sets_the_flag() {
        let mut q = TransferQueue::new(1);
        let a = q.submit(spec(0));
        let started = q.poll_start().unwrap();
        assert!(!started.progress.is_cancelled());

        assert_eq!(q.cancel(a), CancelOutcome::WasRunning);
        assert!(started.progress.is_cancelled());
    }

    #[test]
    fn cancel_unknown_is_a_noop() {
        let mut q = TransferQueue::new(2);
        assert_eq!(q.cancel(TransferId(99)), CancelOutcome::Unknown);
    }

    #[test]
    fn cancel_all_drains_queue_and_flags_running() {
        let mut q = TransferQueue::new(2);
        let a = q.submit(spec(0));
        let b = q.submit(spec(1));
        let c = q.submit(spec(2));
        let started_a = q.poll_start().unwrap();
        let started_b = q.poll_start().unwrap();
        assert_eq!((started_a.id, started_b.id), (a, b));

        let dropped = q.cancel_all();
        assert_eq!(dropped, vec![c]);
        assert!(started_a.progress.is_cancelled());
        assert!(started_b.progress.is_cancelled());
        // Nothing left to start.
        q.finish(a);
        q.finish(b);
        assert!(q.poll_start().is_none());
    }

    #[test]
    fn ids_are_monotonic() {
        let mut q = TransferQueue::new(2);
        let ids: Vec<_> = (0..4).map(|n| q.submit(spec(n))).collect();
        assert_eq!(
            ids,
            vec![TransferId(0), TransferId(1), TransferId(2), TransferId(3)]
        );
    }

    #[test]
    fn running_progress_reflects_byte_counter() {
        let mut q = TransferQueue::new(2);
        q.submit(spec(0));
        let started = q.poll_start().unwrap();
        started.progress.add(128);

        let snapshot: Vec<_> = q.running_progress().collect();
        assert_eq!(snapshot, vec![(started.id, 128)]);
    }
}
