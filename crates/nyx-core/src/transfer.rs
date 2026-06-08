//! The transfer model: identifiers, direction, status and the transfer record.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::RemotePath;

/// A unique, monotonically assigned identifier for a queued transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransferId(pub u64);

/// Whether a transfer moves bytes up to or down from the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferDirection {
    /// Local → remote.
    Upload,
    /// Remote → local.
    Download,
}

/// Whether a transfer moves a single file or a whole directory tree.
///
/// A `Dir` transfer is one user intent - one dock row, one cancel, one collision
/// decision - that the service expands into a recursive walk over the existing
/// single-file copy primitives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferKind {
    /// A single file.
    #[default]
    File,
    /// A directory, copied recursively (parent-before-child).
    Dir,
}

/// The lifecycle state of a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferStatus {
    /// Waiting in the queue.
    Queued,
    /// Parked at the pre-flight gate: the destination exists and the user has
    /// not yet chosen overwrite/skip/cancel.
    AwaitingDecision,
    /// Actively transferring bytes.
    Running,
    /// Finished successfully.
    Completed,
    /// Stopped with an error.
    Failed,
    /// Paused because the connection dropped mid-flight. The bytes already
    /// written are retained and the transfer resumes on reconnect (or can be
    /// cancelled manually). Not a terminal state.
    Interrupted,
    /// Cancelled by the user.
    Cancelled,
    /// Not transferred because the destination already existed and the user (or
    /// the headless default) chose to skip it.
    Skipped,
}

/// How a transfer resolves a destination that already exists.
///
/// `None` on a [`TransferSpec`](crate)'s policy slot means "ask the user"; a
/// resolved choice short-circuits the prompt (e.g. a pre-answered "apply to
/// all" batch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollisionChoice {
    /// Truncate and overwrite the existing destination.
    Overwrite,
    /// Leave the existing destination untouched; the transfer ends `Skipped`.
    Skip,
    /// Abort the transfer; it ends `Cancelled`.
    Cancel,
}

/// Whether a per-entry issue in a folder transfer was a hard failure (the copy
/// errored mid-flight) or a deliberate skip (a symlink, special file, or
/// non-representable name the walk declined to copy in the first place).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryOutcomeKind {
    /// The copy was attempted and errored (e.g. permission denied, unreadable).
    Failed,
    /// The entry was never copied (symlink, special file, non-UTF-8 name).
    Skipped,
}

/// One entry a recursive folder transfer could not copy, kept so a partial
/// success is *diagnosable* rather than a bare tally: the path (relative to the
/// transfer root) and a credential-free reason.
///
/// Reasons come from `io::Error`/[`NyxError`](crate::NyxError) text or the walk's
/// skip classification - both are filesystem paths and OS error strings, so a
/// secret can never appear here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryIssue {
    /// Path relative to the transfer root, e.g. `"sub/report.txt"`.
    pub rel: String,
    /// Whether the entry failed mid-copy or was skipped during enumeration.
    pub kind: EntryOutcomeKind,
    /// A short, credential-free reason (e.g. `"permission denied"`,
    /// `"symlink skipped"`).
    pub reason: String,
}

impl EntryIssue {
    /// A skip discovered during enumeration (a walk).
    pub fn skipped(rel: String, reason: impl Into<String>) -> Self {
        Self {
            rel,
            kind: EntryOutcomeKind::Skipped,
            reason: reason.into(),
        }
    }

    /// A failure caught while copying an entry.
    pub fn failed(rel: String, reason: impl Into<String>) -> Self {
        Self {
            rel,
            kind: EntryOutcomeKind::Failed,
            reason: reason.into(),
        }
    }
}

/// The per-entry report attached to a completed-with-issues folder transfer.
///
/// `failed`/`skipped` are exact full counts; `issues` is a **capped** retained
/// list (see [`EntryIssue`]). When `issues.len()` is short of `failed + skipped`
/// the tail was truncated - [`truncated`](Self::truncated) reports by how many,
/// so the cap is never a silent drop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferReport {
    /// Exact count of entries that failed mid-copy.
    pub failed: u64,
    /// Exact count of entries skipped during enumeration.
    pub skipped: u64,
    /// The retained per-entry detail, capped - may be shorter than the counts.
    pub issues: Vec<EntryIssue>,
}

impl TransferReport {
    /// One-line at-a-glance summary, e.g. `"2 failed, 1 skipped"`; `None` when
    /// the transfer was clean (no failures, no skips).
    pub fn summary(&self) -> Option<String> {
        let mut notes = Vec::new();
        if self.failed > 0 {
            notes.push(format!("{} failed", self.failed));
        }
        if self.skipped > 0 {
            notes.push(format!("{} skipped", self.skipped));
        }
        (!notes.is_empty()).then(|| notes.join(", "))
    }

    /// Whether the report carries any failure or skip at all.
    pub fn has_issues(&self) -> bool {
        self.failed > 0 || self.skipped > 0
    }

    /// How many issues the retention cap dropped from `issues` (full counts stay
    /// exact, so this is `failed + skipped` minus what we kept).
    pub fn truncated(&self) -> u64 {
        (self.failed + self.skipped).saturating_sub(self.issues.len() as u64)
    }
}

/// A single queued / running transfer and its progress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transfer {
    /// Stable identifier for this transfer.
    pub id: TransferId,
    /// Upload or download.
    pub direction: TransferDirection,
    /// File or whole-directory transfer.
    #[serde(default)]
    pub kind: TransferKind,
    /// The remote-side path.
    pub remote_path: RemotePath,
    /// The local-side path (display form).
    pub local_path: String,
    /// Total size in bytes, if known up front.
    pub total_bytes: Option<u64>,
    /// Bytes transferred so far.
    pub transferred_bytes: u64,
    /// Current lifecycle state.
    pub status: TransferStatus,
}

impl Transfer {
    /// Fraction complete in `0.0..=1.0`, or `None` when the total is unknown.
    pub fn progress(&self) -> Option<f32> {
        match self.total_bytes {
            Some(0) | None => None,
            Some(total) => Some((self.transferred_bytes as f32 / total as f32).clamp(0.0, 1.0)),
        }
    }
}

/// A cheap fingerprint of a transfer's source file, captured when a copy first
/// starts and re-checked before a resume. If the source changed under us during
/// the outage (different size or mtime), splicing the remaining bytes onto the
/// partial destination would corrupt it - so a mismatch forces a full restart
/// from zero instead. `mtime` is best-effort: a `None` on either side (a server
/// that doesn't report it, a protocol that can't stat) is treated as "can't
/// verify", which also forces a restart. Never resume on doubt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMeta {
    /// Source size in bytes at capture time.
    pub size: u64,
    /// Source modification time (seconds), if the source could report it.
    pub mtime: Option<u64>,
}

/// A shared progress + cancel handle carried into a running transfer's copy loop.
///
/// One handle is held by both the copy task (which `add`s bytes per chunk and
/// checks `is_cancelled` between chunks) and the scheduler/service (which reads
/// `transferred` for progress sampling and flips `cancel` on request). All
/// operations use `Relaxed` ordering: a progress read that is one chunk stale is
/// fine for a progress bar, and the cancel flag is a one-way latch.
#[derive(Debug, Clone, Default)]
pub struct TransferProgress {
    transferred: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

impl TransferProgress {
    /// Record `n` more transferred bytes.
    pub fn add(&self, n: u64) {
        self.transferred.fetch_add(n, Ordering::Relaxed);
    }

    /// Seed the byte counter to `n`. Used when resuming a transfer from an
    /// offset: the already-written bytes count toward progress so the dock's
    /// `%` / speed math stays correct from the first sample.
    pub fn seed(&self, n: u64) {
        self.transferred.store(n, Ordering::Relaxed);
    }

    /// Cumulative bytes transferred so far.
    pub fn transferred(&self) -> u64 {
        self.transferred.load(Ordering::Relaxed)
    }

    /// Request cancellation; the copy loop notices between chunks.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer(total: Option<u64>, done: u64) -> Transfer {
        Transfer {
            id: TransferId(1),
            direction: TransferDirection::Download,
            kind: TransferKind::File,
            remote_path: "/r".into(),
            local_path: "/l".into(),
            total_bytes: total,
            transferred_bytes: done,
            status: TransferStatus::Running,
        }
    }

    #[test]
    fn progress_is_fraction_complete() {
        assert_eq!(transfer(Some(100), 50).progress(), Some(0.5));
    }

    #[test]
    fn progress_is_none_when_total_unknown_or_zero() {
        assert_eq!(transfer(None, 10).progress(), None);
        assert_eq!(transfer(Some(0), 0).progress(), None);
    }

    #[test]
    fn progress_is_clamped() {
        assert_eq!(transfer(Some(100), 250).progress(), Some(1.0));
    }

    #[test]
    fn transfer_progress_add_read_cancel() {
        let p = TransferProgress::default();
        assert_eq!(p.transferred(), 0);
        assert!(!p.is_cancelled());

        p.add(40);
        p.add(2);
        assert_eq!(p.transferred(), 42);

        // A clone shares the same counter + flag.
        let other = p.clone();
        other.cancel();
        assert!(p.is_cancelled());
        assert_eq!(other.transferred(), 42);
    }
}
