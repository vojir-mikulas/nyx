//! Transfer queue for Nyx.
//!
//! A **sans-IO scheduler**: pure policy with no tokio, no protocol and no
//! spawning. It owns the concurrency cap, the queue of pending specs, id
//! allocation and - per running transfer - a shared [`TransferProgress`] handle
//! (a byte counter + a one-way cancel flag). The service drives it: it `submit`s
//! specs, `poll_start`s to promote queued transfers into running slots (spawning
//! the actual copy tasks), `finish`es them on completion and `cancel`s on
//! request. Keeping the queue free of I/O makes the policy unit-testable without
//! a runtime or a server.
//!
//! ## Path-lock policy
//!
//! Every live transfer (`Queued` / `AwaitingDecision` / `Running`) holds a lock
//! on **both** its remote and local path. [`submit`](TransferQueue::submit)
//! **rejects** a second transfer whose remote or local path is already locked,
//! and [`is_remote_locked`](TransferQueue::is_remote_locked) lets the service
//! reject a mutating op (`Remove` / `Rename`) against a path with a live
//! transfer. This is the safe, predictable first cut: a duplicate active op on a
//! locked path is refused (surfacing the conflict) rather than racing - a
//! delete-during-download or a double-upload to one path can't corrupt state.
//! Independent paths still run concurrently up to the cap. Locks release on
//! every terminal transition (finish / cancel / skip / failed). Richer per-path
//! queueing (auto-serialize same-path ops instead of rejecting) is deferred.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use nyx_core::{
    CollisionChoice, RemotePath, SourceMeta, TransferDirection, TransferId, TransferKind,
    TransferProgress,
};

/// A `submit` (or a mutating op against a locked path) was rejected because the
/// path already has a live transfer. See the crate-level path-lock policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathInUse;

/// What the service hands in to enqueue a transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferSpec {
    /// Upload or download.
    pub direction: TransferDirection,
    /// A single file or a whole directory tree. A `Dir` lock covers the root and
    /// every path beneath it (the path-lock policy).
    pub kind: TransferKind,
    /// The remote-side path.
    pub remote: RemotePath,
    /// The local-side path.
    pub local: PathBuf,
    /// Pre-resolved collision policy. `None` means "ask the user when the
    /// destination already exists"; a resolved choice (e.g. an "apply to all"
    /// stamp) skips the prompt round-trip.
    pub on_collision: Option<CollisionChoice>,
    /// Byte offset to resume from; `0` means start fresh. Set when the queue
    /// re-admits an interrupted transfer - the copy only actually seeks to it if
    /// the source is verifiably unchanged, otherwise it restarts from `0`.
    pub resume_from: u64,
    /// The source fingerprint captured on the first run, carried across an
    /// interruption so a resume can confirm the source hasn't changed under us.
    pub source_meta: Option<SourceMeta>,
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
    /// The id was still queued (or parked awaiting a decision); it was dropped
    /// and will never start. No copy task ran for it.
    WasQueued,
    /// The id was running; its cancel flag was set (the copy winds down).
    WasRunning,
    /// The id is unknown (already finished, or never existed).
    Unknown,
}

/// The transfers a [`TransferQueue::resolve`] decision terminated outright (no
/// copy ran), so the service can emit their terminal events. `Overwrite`-resolved
/// transfers are *not* listed: they were re-queued and surface via the normal
/// start path instead.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// Transfers that end `Skipped` (the destination is left untouched).
    pub skipped: Vec<TransferId>,
    /// Transfers that end `Cancelled`.
    pub cancelled: Vec<TransferId>,
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
    /// Running transfers, by id, with their spec and shared progress handle. The
    /// spec is retained so a collision can [`park`](Self::park) the item.
    running: HashMap<TransferId, (TransferSpec, TransferProgress)>,
    /// Transfers parked at the pre-flight gate, awaiting a collision decision.
    /// A parked item holds no running slot.
    awaiting: HashMap<TransferId, TransferSpec>,
    /// Transfers paused by a connection loss, retaining their spec (with the
    /// resume offset stamped on it). They hold a running slot for no one but keep
    /// their path lock - [`readmit_interrupted`](Self::readmit_interrupted) puts
    /// them back on the queue on reconnect.
    interrupted: HashMap<TransferId, TransferSpec>,
    /// Per-transfer record of the paths it locked (and the kind, which decides
    /// whether the lock covers a subtree), so a terminal transition releases
    /// exactly those. The conflict checks iterate these directly.
    locks: HashMap<TransferId, LockEntry>,
}

/// One live transfer's path lock. A `Dir` lock covers its root **and every
/// descendant**; a `File` lock is the exact path only.
struct LockEntry {
    remote: RemotePath,
    local: PathBuf,
    kind: TransferKind,
}

/// Whether the set an owner of `kind` rooted at `owner` covers includes `query`:
/// a directory owner covers its whole subtree, a file owner only its exact path.
fn remote_covers(owner: &RemotePath, kind: TransferKind, query: &RemotePath) -> bool {
    match kind {
        TransferKind::Dir => query.is_within(owner),
        TransferKind::File => query == owner,
    }
}

/// Local-side counterpart of [`remote_covers`] (`Path::starts_with` compares
/// whole components).
fn local_covers(owner: &Path, kind: TransferKind, query: &Path) -> bool {
    match kind {
        TransferKind::Dir => query.starts_with(owner),
        TransferKind::File => query == owner,
    }
}

impl TransferQueue {
    /// Create an empty queue with the given concurrency cap.
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            next_id: 0,
            queued: VecDeque::new(),
            running: HashMap::new(),
            awaiting: HashMap::new(),
            interrupted: HashMap::new(),
            locks: HashMap::new(),
        }
    }

    /// Set the concurrency cap (clamped to at least 1). The queue stays
    /// protocol-agnostic; the caller chooses the number - e.g. the service drops
    /// it to 1 for FTP/FTPS, whose single control connection serializes every op,
    /// so the queue never starts a transfer that would only stall behind another.
    /// Lowering the cap below the number already running starts nothing new until
    /// the running set drains back under it; it never interrupts a live transfer.
    pub fn set_cap(&mut self, cap: usize) {
        self.cap = cap.max(1);
    }

    /// Assign an id, lock the spec's paths and push it onto the queue, returning
    /// the new id - unless the remote or local path already has a live transfer,
    /// in which case it is **rejected** with [`PathInUse`] (the path-lock policy).
    pub fn submit(&mut self, spec: TransferSpec) -> Result<TransferId, PathInUse> {
        if self.path_conflict(&spec) {
            return Err(PathInUse);
        }
        let id = TransferId(self.next_id);
        self.next_id += 1;
        self.lock(id, &spec);
        self.queued.push_back((id, spec));
        Ok(id)
    }

    /// Whether `spec`'s remote or local path overlaps any live transfer's locked
    /// set. Two transfers conflict when either's path falls inside the other's
    /// covered set, so a directory transfer blocks (and is blocked by) anything
    /// in its subtree, not just its exact root.
    fn path_conflict(&self, spec: &TransferSpec) -> bool {
        self.locks.values().any(|lock| {
            remote_covers(&lock.remote, lock.kind, &spec.remote)
                || remote_covers(&spec.remote, spec.kind, &lock.remote)
                || local_covers(&lock.local, lock.kind, &spec.local)
                || local_covers(&spec.local, spec.kind, &lock.local)
        })
    }

    /// Whether a remote path currently has a live transfer - the service checks
    /// this to reject a mutating op (`Remove` / `Rename`) that would race a copy.
    ///
    /// `path` is treated as covering its own subtree (a `Remove`/`Rename` of a
    /// directory disturbs every transfer beneath it), and a live **dir** lock
    /// covers its descendants - so deleting a child of a folder being downloaded,
    /// or an ancestor of a file being transferred, is both caught.
    pub fn is_remote_locked(&self, path: &RemotePath) -> bool {
        self.locks.values().any(|lock| {
            remote_covers(&lock.remote, lock.kind, path)
                || remote_covers(path, TransferKind::Dir, &lock.remote)
        })
    }

    /// Record `spec`'s path locks for `id`.
    fn lock(&mut self, id: TransferId, spec: &TransferSpec) {
        self.locks.insert(
            id,
            LockEntry {
                remote: spec.remote.clone(),
                local: spec.local.clone(),
                kind: spec.kind,
            },
        );
    }

    /// Release `id`'s path locks (a no-op if it held none). Called from every
    /// terminal transition so a path can never leak a permanent lock.
    fn release(&mut self, id: TransferId) {
        self.locks.remove(&id);
    }

    /// If a slot is free and a transfer is queued, promote the oldest one into a
    /// running slot and return its [`Started`] handle; otherwise `None`.
    ///
    /// "Running" here means "claimed a slot"; the service still runs the
    /// pre-flight collision gate before any bytes move, and may [`park`](Self::park)
    /// the item back out if the destination already exists.
    pub fn poll_start(&mut self) -> Option<Started> {
        if self.running.len() >= self.cap {
            return None;
        }
        let (id, spec) = self.queued.pop_front()?;
        let progress = TransferProgress::default();
        self.running.insert(id, (spec.clone(), progress.clone()));
        Some(Started { id, spec, progress })
    }

    /// Move a running transfer to the `AwaitingDecision` state (the pre-flight
    /// gate hit an existing destination), freeing its slot. Returns the parked
    /// spec so the service can build the collision event; `None` if the id was
    /// not running.
    pub fn park(&mut self, id: TransferId) -> Option<TransferSpec> {
        let (spec, _progress) = self.running.remove(&id)?;
        self.awaiting.insert(id, spec.clone());
        Some(spec)
    }

    /// Apply a collision decision. With `apply_to_all`, the same `choice` is
    /// applied to **every** currently-parked transfer and stamped onto every
    /// still-queued transfer (so they won't prompt when they reach the gate).
    ///
    /// `Overwrite` re-queues the parked item (with the policy resolved) so it
    /// runs next; `Skip`/`Cancel` terminate it immediately - the returned
    /// [`Resolution`] lists those for the service to announce.
    pub fn resolve(
        &mut self,
        id: TransferId,
        choice: CollisionChoice,
        apply_to_all: bool,
    ) -> Resolution {
        let targets: Vec<TransferId> = if apply_to_all {
            self.awaiting.keys().copied().collect()
        } else {
            vec![id]
        };
        let mut out = Resolution::default();
        for tid in targets {
            let Some(mut spec) = self.awaiting.remove(&tid) else {
                continue;
            };
            match choice {
                CollisionChoice::Overwrite => {
                    spec.on_collision = Some(CollisionChoice::Overwrite);
                    // Re-admit at the front so the resolved item runs next.
                    self.queued.push_front((tid, spec));
                }
                // Skip/Cancel terminate the item here (no copy runs) - release
                // its lock now, since no `finish` will follow.
                CollisionChoice::Skip => {
                    self.release(tid);
                    out.skipped.push(tid);
                }
                CollisionChoice::Cancel => {
                    self.release(tid);
                    out.cancelled.push(tid);
                }
            }
        }
        if apply_to_all {
            // Stamp still-queued items so a later collision resolves silently.
            // (Don't overwrite an item we just re-admitted with its own policy.)
            for (_, spec) in self.queued.iter_mut() {
                if spec.on_collision.is_none() {
                    spec.on_collision = Some(choice);
                }
            }
        }
        out
    }

    /// Drop a transfer from the running set (it reached a terminal state) and
    /// release its path locks.
    pub fn finish(&mut self, id: TransferId) {
        self.running.remove(&id);
        self.release(id);
    }

    /// Cancel a transfer by id. A queued or parked transfer is dropped
    /// immediately (no task ran); a running one has its cancel flag set (the
    /// copy loop notices and winds down, then the service `finish`es it through
    /// the normal terminal path).
    pub fn cancel(&mut self, id: TransferId) -> CancelOutcome {
        if let Some(pos) = self.queued.iter().position(|(qid, _)| *qid == id) {
            self.queued.remove(pos);
            // Dropped before it started - no `finish` will follow, so unlock now.
            self.release(id);
            return CancelOutcome::WasQueued;
        }
        if self.awaiting.remove(&id).is_some() {
            self.release(id);
            return CancelOutcome::WasQueued;
        }
        if self.interrupted.remove(&id).is_some() {
            // Paused by a loss and never resumed - drop it like a queued one.
            self.release(id);
            return CancelOutcome::WasQueued;
        }
        if let Some((_, progress)) = self.running.get(&id) {
            // The copy winds down and the service calls `finish`, which unlocks.
            progress.cancel();
            return CancelOutcome::WasRunning;
        }
        CancelOutcome::Unknown
    }

    /// Cancel everything: flag every running transfer and drain the queued and
    /// parked transfers. Returns the ids of the dropped queued/parked transfers
    /// (the service emits a terminal `Cancelled` event for each, since no task
    /// ever ran for them).
    pub fn cancel_all(&mut self) -> Vec<TransferId> {
        for (_, progress) in self.running.values() {
            progress.cancel();
        }
        let mut dropped: Vec<TransferId> = self.queued.drain(..).map(|(id, _)| id).collect();
        dropped.extend(self.awaiting.drain().map(|(id, _)| id));
        dropped.extend(self.interrupted.drain().map(|(id, _)| id));
        // The dropped queued/parked/interrupted items get no `finish`, so release
        // their locks now; the flagged running ones release when the service
        // `finish`es them.
        for id in &dropped {
            self.release(*id);
        }
        dropped
    }

    /// Move the **pending** transfers (queued + parked) into the Interrupted
    /// holding state on a connection loss, **keeping their locks**, and return
    /// their ids. They hadn't started, so their resume offset is `0` (a fresh
    /// restart on reconnect). The running ones are interrupted individually via
    /// [`interrupt`](Self::interrupt) as their copy tasks notice the drop.
    pub fn interrupt_pending(&mut self) -> Vec<TransferId> {
        let mut moved: Vec<TransferId> = Vec::new();
        let queued: Vec<(TransferId, TransferSpec)> = self.queued.drain(..).collect();
        for (id, mut spec) in queued {
            spec.resume_from = 0;
            self.interrupted.insert(id, spec);
            moved.push(id);
        }
        let awaiting: Vec<(TransferId, TransferSpec)> = self.awaiting.drain().collect();
        for (id, mut spec) in awaiting {
            spec.resume_from = 0;
            self.interrupted.insert(id, spec);
            moved.push(id);
        }
        moved
    }

    /// Move a **running** transfer into the Interrupted holding state, stamping
    /// the bytes-done `watermark` as its resume offset and recording the source
    /// fingerprint for the resume guard. Keeps its path lock. Returns `false` if
    /// the id wasn't running (already finished, or never started).
    pub fn interrupt(
        &mut self,
        id: TransferId,
        watermark: u64,
        source_meta: Option<nyx_core::SourceMeta>,
    ) -> bool {
        let Some((mut spec, _progress)) = self.running.remove(&id) else {
            return false;
        };
        spec.resume_from = watermark;
        spec.source_meta = source_meta;
        self.interrupted.insert(id, spec);
        true
    }

    /// Re-admit every interrupted transfer to the queue (at the front, in their
    /// original submission order) for a resume after reconnect. Each is stamped
    /// `Overwrite` so the pre-flight gate doesn't re-prompt for the partial it is
    /// resuming onto. Returns the number re-admitted.
    pub fn readmit_interrupted(&mut self) -> usize {
        if self.interrupted.is_empty() {
            return 0;
        }
        // Ids are monotonic, so sorting by id restores submission order.
        let mut entries: Vec<(TransferId, TransferSpec)> = self.interrupted.drain().collect();
        entries.sort_by_key(|(id, _)| *id);
        let count = entries.len();
        // Push in reverse so the final front order matches submission order.
        for (id, mut spec) in entries.into_iter().rev() {
            spec.on_collision = Some(CollisionChoice::Overwrite);
            self.queued.push_front((id, spec));
        }
        count
    }

    /// Drop every interrupted transfer (e.g. on connecting to a *different*
    /// profile, or on disconnect), releasing their locks and returning their ids
    /// so the service can announce them cancelled.
    pub fn drain_interrupted(&mut self) -> Vec<TransferId> {
        let ids: Vec<TransferId> = self.interrupted.drain().map(|(id, _)| id).collect();
        for id in &ids {
            self.release(*id);
        }
        ids
    }

    /// Each running transfer's `(id, bytes transferred so far)`, for the
    /// service's progress ticker.
    pub fn running_progress(&self) -> impl Iterator<Item = (TransferId, u64)> + '_ {
        self.running
            .iter()
            .map(|(id, (_, p))| (*id, p.transferred()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(n: u64) -> TransferSpec {
        TransferSpec {
            direction: TransferDirection::Download,
            kind: TransferKind::File,
            remote: RemotePath::new(format!("/r/{n}")),
            local: PathBuf::from(format!("/l/{n}")),
            on_collision: None,
            resume_from: 0,
            source_meta: None,
        }
    }

    /// A directory transfer rooted at `remote` / `local`.
    fn dir_spec(remote: &str, local: &str) -> TransferSpec {
        TransferSpec {
            direction: TransferDirection::Download,
            kind: TransferKind::Dir,
            remote: RemotePath::new(remote),
            local: PathBuf::from(local),
            on_collision: None,
            resume_from: 0,
            source_meta: None,
        }
    }

    #[test]
    fn cap_is_enforced_and_backfills_on_finish() {
        let mut q = TransferQueue::new(3);
        let ids: Vec<_> = (0..5).map(|n| q.submit(spec(n)).unwrap()).collect();

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
        let a = q.submit(spec(0)).unwrap();
        let b = q.submit(spec(1)).unwrap();

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
        let a = q.submit(spec(0)).unwrap();
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
    fn set_cap_gates_admission_without_interrupting_running() {
        let mut q = TransferQueue::new(3);
        let a = q.submit(spec(0)).unwrap();
        let _b = q.submit(spec(1)).unwrap();
        // One running, then tighten to 1: the queued one must not start, and the
        // running one is untouched.
        assert_eq!(q.poll_start().unwrap().id, a);
        q.set_cap(1);
        assert!(
            q.poll_start().is_none(),
            "cap=1 with one running starts nothing"
        );
        // Draining back under the cap lets the queued one through.
        q.finish(a);
        assert!(q.poll_start().is_some());
    }

    #[test]
    fn set_cap_clamps_to_at_least_one() {
        let mut q = TransferQueue::new(2);
        q.set_cap(0);
        let _a = q.submit(spec(0)).unwrap();
        let _b = q.submit(spec(1)).unwrap();
        // Clamped to 1: exactly one starts, the next waits.
        assert!(q.poll_start().is_some());
        assert!(q.poll_start().is_none());
    }

    #[test]
    fn cancel_all_drains_queue_and_flags_running() {
        let mut q = TransferQueue::new(2);
        let a = q.submit(spec(0)).unwrap();
        let b = q.submit(spec(1)).unwrap();
        let c = q.submit(spec(2)).unwrap();
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
        let ids: Vec<_> = (0..4).map(|n| q.submit(spec(n)).unwrap()).collect();
        assert_eq!(
            ids,
            vec![TransferId(0), TransferId(1), TransferId(2), TransferId(3)]
        );
    }

    #[test]
    fn park_frees_the_slot_and_resolve_overwrite_runs_next() {
        let mut q = TransferQueue::new(1);
        let a = q.submit(spec(0)).unwrap();
        let b = q.submit(spec(1)).unwrap();
        // `a` claims the only slot; `b` waits.
        let started_a = q.poll_start().unwrap();
        assert_eq!(started_a.id, a);
        assert!(q.poll_start().is_none());

        // The gate parks `a` (its destination exists) - the slot frees and `b`
        // can start while `a` awaits a decision.
        assert!(q.park(a).is_some());
        let started_b = q.poll_start().unwrap();
        assert_eq!(started_b.id, b);

        // Overwrite re-admits `a` at the front; once a slot frees it runs next,
        // and its resolved policy is stamped.
        let res = q.resolve(a, CollisionChoice::Overwrite, false);
        assert_eq!(res, Resolution::default());
        q.finish(b);
        let restarted = q.poll_start().unwrap();
        assert_eq!(restarted.id, a);
        assert_eq!(
            restarted.spec.on_collision,
            Some(CollisionChoice::Overwrite)
        );
    }

    #[test]
    fn resolve_skip_and_cancel_terminate_without_running() {
        let mut q = TransferQueue::new(2);
        let a = q.submit(spec(0)).unwrap();
        let b = q.submit(spec(1)).unwrap();
        q.poll_start().unwrap();
        q.poll_start().unwrap();
        q.park(a);
        q.park(b);

        assert_eq!(
            q.resolve(a, CollisionChoice::Skip, false),
            Resolution {
                skipped: vec![a],
                cancelled: vec![],
            }
        );
        assert_eq!(
            q.resolve(b, CollisionChoice::Cancel, false),
            Resolution {
                skipped: vec![],
                cancelled: vec![b],
            }
        );
        // Both left the queue entirely; nothing runs.
        assert!(q.poll_start().is_none());
    }

    #[test]
    fn apply_to_all_stamps_parked_and_queued_items() {
        let mut q = TransferQueue::new(2);
        let a = q.submit(spec(0)).unwrap();
        let b = q.submit(spec(1)).unwrap();
        let c = q.submit(spec(2)).unwrap(); // stays queued (cap is 2)
        q.poll_start().unwrap();
        q.poll_start().unwrap();
        q.park(a);
        q.park(b);

        // Skip-all: both parked items end Skipped in one decision…
        let res = q.resolve(a, CollisionChoice::Skip, true);
        assert_eq!(res.skipped.len(), 2);
        assert!(res.skipped.contains(&a) && res.skipped.contains(&b));

        // …and the still-queued `c` is stamped, so it won't prompt at the gate.
        let started_c = q.poll_start().unwrap();
        assert_eq!(started_c.id, c);
        assert_eq!(started_c.spec.on_collision, Some(CollisionChoice::Skip));
    }

    #[test]
    fn cancel_parked_drops_it() {
        let mut q = TransferQueue::new(1);
        let a = q.submit(spec(0)).unwrap();
        q.poll_start().unwrap();
        q.park(a);
        assert_eq!(q.cancel(a), CancelOutcome::WasQueued);
        assert_eq!(q.cancel(a), CancelOutcome::Unknown);
    }

    #[test]
    fn cancel_all_drains_parked_too() {
        let mut q = TransferQueue::new(2);
        let a = q.submit(spec(0)).unwrap();
        let b = q.submit(spec(1)).unwrap();
        q.poll_start().unwrap();
        q.poll_start().unwrap();
        q.park(a);
        let dropped = q.cancel_all();
        // `a` was parked, `b` still running (flagged, not dropped here).
        assert_eq!(dropped, vec![a]);
        assert!(q.poll_start().is_none());
        let _ = b;
    }

    #[test]
    fn running_progress_reflects_byte_counter() {
        let mut q = TransferQueue::new(2);
        q.submit(spec(0)).unwrap();
        let started = q.poll_start().unwrap();
        started.progress.add(128);

        let snapshot: Vec<_> = q.running_progress().collect();
        assert_eq!(snapshot, vec![(started.id, 128)]);
    }

    #[test]
    fn second_submit_for_a_live_path_is_rejected_then_freed() {
        let mut q = TransferQueue::new(2);
        let a = q.submit(spec(0)).unwrap();
        // A second transfer for the same paths is rejected while the first lives.
        assert_eq!(q.submit(spec(0)), Err(PathInUse));
        // The remote path is reported locked, so a Remove/Rename guard catches it.
        assert!(q.is_remote_locked(&RemotePath::new("/r/0")));
        // Once the first reaches a terminal state the path frees up.
        q.poll_start().unwrap();
        q.finish(a);
        assert!(!q.is_remote_locked(&RemotePath::new("/r/0")));
        assert!(q.submit(spec(0)).is_ok());
    }

    #[test]
    fn a_partial_path_overlap_still_collides() {
        // Same remote but different local (two downloads of one file to two
        // places) - still a conflict; same local but different remote likewise.
        let mut q = TransferQueue::new(2);
        q.submit(spec(0)).unwrap();
        let same_remote = TransferSpec {
            local: PathBuf::from("/l/other"),
            ..spec(0)
        };
        assert_eq!(q.submit(same_remote), Err(PathInUse));
        let same_local = TransferSpec {
            remote: RemotePath::new("/r/other"),
            ..spec(0)
        };
        assert_eq!(q.submit(same_local), Err(PathInUse));
    }

    #[test]
    fn dir_lock_covers_descendants_but_not_siblings() {
        let mut q = TransferQueue::new(2);
        // A folder download locks /r/dir (and its whole subtree).
        q.submit(dir_spec("/r/dir", "/l/dir")).unwrap();

        // A Remove/Rename of a child is blocked; the root itself is blocked…
        assert!(q.is_remote_locked(&RemotePath::new("/r/dir")));
        assert!(q.is_remote_locked(&RemotePath::new("/r/dir/child.txt")));
        assert!(q.is_remote_locked(&RemotePath::new("/r/dir/sub/deep")));
        // …a sibling is not, and a same-prefix-but-different name is not.
        assert!(!q.is_remote_locked(&RemotePath::new("/r/other")));
        assert!(!q.is_remote_locked(&RemotePath::new("/r/dirx")));
    }

    #[test]
    fn removing_an_ancestor_of_a_file_transfer_is_blocked() {
        // A plain file transfer on /r/a/b/c.txt must block a delete of /r/a/b.
        let mut q = TransferQueue::new(2);
        q.submit(TransferSpec {
            remote: RemotePath::new("/r/a/b/c.txt"),
            ..spec(0)
        })
        .unwrap();
        assert!(q.is_remote_locked(&RemotePath::new("/r/a/b")));
        assert!(q.is_remote_locked(&RemotePath::new("/r/a/b/c.txt")));
        // A sibling file under the same parent is free.
        assert!(!q.is_remote_locked(&RemotePath::new("/r/a/b/d.txt")));
    }

    #[test]
    fn submitting_into_a_locked_dir_subtree_is_rejected() {
        let mut q = TransferQueue::new(3);
        q.submit(dir_spec("/r/dir", "/l/dir")).unwrap();
        // A file transfer whose remote falls under the locked dir conflicts…
        let inside = TransferSpec {
            remote: RemotePath::new("/r/dir/file.txt"),
            local: PathBuf::from("/l/elsewhere"),
            ..spec(0)
        };
        assert_eq!(q.submit(inside), Err(PathInUse));
        // …and a local destination under the locked local root also conflicts.
        let inside_local = TransferSpec {
            remote: RemotePath::new("/r/elsewhere"),
            local: PathBuf::from("/l/dir/file.txt"),
            ..spec(0)
        };
        assert_eq!(q.submit(inside_local), Err(PathInUse));
        // A fully independent path is fine.
        assert!(q.submit(spec(9)).is_ok());
    }

    #[test]
    fn independent_paths_never_collide() {
        let mut q = TransferQueue::new(3);
        assert!(q.submit(spec(0)).is_ok());
        assert!(q.submit(spec(1)).is_ok());
        assert!(q.submit(spec(2)).is_ok());
    }

    #[test]
    fn lock_releases_on_cancel_and_resolve_skip() {
        // Cancel a queued transfer → its path frees.
        let mut q = TransferQueue::new(1);
        let a = q.submit(spec(0)).unwrap();
        assert_eq!(q.cancel(a), CancelOutcome::WasQueued);
        assert!(q.submit(spec(0)).is_ok());

        // Skip-resolving a parked transfer → its path frees.
        let mut q = TransferQueue::new(1);
        let b = q.submit(spec(1)).unwrap();
        q.poll_start().unwrap();
        q.park(b);
        q.resolve(b, CollisionChoice::Skip, false);
        assert!(!q.is_remote_locked(&RemotePath::new("/r/1")));
        assert!(q.submit(spec(1)).is_ok());
    }

    #[test]
    fn overwrite_keeps_the_lock_until_finish() {
        let mut q = TransferQueue::new(1);
        let a = q.submit(spec(0)).unwrap();
        q.poll_start().unwrap();
        q.park(a);
        // Re-queued for overwrite: the path stays locked (still a live transfer).
        q.resolve(a, CollisionChoice::Overwrite, false);
        assert!(q.is_remote_locked(&RemotePath::new("/r/0")));
        let restarted = q.poll_start().unwrap();
        q.finish(restarted.id);
        assert!(!q.is_remote_locked(&RemotePath::new("/r/0")));
    }

    #[test]
    fn interrupt_pending_holds_queued_and_parked_keeping_locks() {
        let mut q = TransferQueue::new(1);
        let a = q.submit(spec(0)).unwrap(); // will run
        let b = q.submit(spec(1)).unwrap(); // stays queued
        let started_a = q.poll_start().unwrap();
        assert_eq!(started_a.id, a);

        let held = q.interrupt_pending();
        assert_eq!(held, vec![b]);
        // The pending one is now interrupted but still locks its path (it will
        // resume on reconnect), and the running one is untouched.
        assert!(q.is_remote_locked(&RemotePath::new("/r/0")));
        assert!(q.is_remote_locked(&RemotePath::new("/r/1")));
        // It can't start while merely interrupted…
        assert!(q.poll_start().is_none());
        // …until it's re-admitted, which restores it to the queue (front).
        assert_eq!(q.readmit_interrupted(), 1);
        q.finish(a);
        let resumed = q.poll_start().unwrap();
        assert_eq!(resumed.id, b);
        assert_eq!(resumed.spec.on_collision, Some(CollisionChoice::Overwrite));
    }

    #[test]
    fn interrupt_running_records_watermark_and_resumes() {
        let mut q = TransferQueue::new(2);
        let a = q.submit(spec(0)).unwrap();
        let started = q.poll_start().unwrap();
        assert_eq!(started.id, a);

        let meta = nyx_core::SourceMeta {
            size: 1000,
            mtime: Some(42),
        };
        assert!(q.interrupt(a, 256, Some(meta)));
        // It's out of the running set but keeps its lock.
        assert!(q.is_remote_locked(&RemotePath::new("/r/0")));
        assert!(q.running_progress().next().is_none());

        // Re-admit restores it with the resume offset + source fingerprint.
        assert_eq!(q.readmit_interrupted(), 1);
        let resumed = q.poll_start().unwrap();
        assert_eq!(resumed.id, a);
        assert_eq!(resumed.spec.resume_from, 256);
        assert_eq!(resumed.spec.source_meta, Some(meta));
    }

    #[test]
    fn readmit_preserves_submission_order() {
        let mut q = TransferQueue::new(3);
        let ids: Vec<_> = (0..3).map(|n| q.submit(spec(n)).unwrap()).collect();
        for _ in 0..3 {
            q.poll_start().unwrap();
        }
        // Interrupt all three running transfers out of order…
        assert!(q.interrupt(ids[2], 10, None));
        assert!(q.interrupt(ids[0], 20, None));
        assert!(q.interrupt(ids[1], 30, None));
        // …re-admit, and they come back in original submission order.
        assert_eq!(q.readmit_interrupted(), 3);
        let order: Vec<_> = std::iter::from_fn(|| q.poll_start())
            .map(|s| s.id)
            .collect();
        assert_eq!(order, ids);
    }

    #[test]
    fn cancel_interrupted_drops_it_and_frees_the_path() {
        let mut q = TransferQueue::new(1);
        let a = q.submit(spec(0)).unwrap();
        q.poll_start().unwrap();
        assert!(q.interrupt(a, 64, None));
        assert_eq!(q.cancel(a), CancelOutcome::WasQueued);
        assert!(!q.is_remote_locked(&RemotePath::new("/r/0")));
        assert_eq!(q.readmit_interrupted(), 0);
    }
}
