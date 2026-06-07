//! Bridge from OS drag-out promises to the normal download pipeline.
//!
//! When the user drags a remote file out to Finder, [`nyx_drag`] starts a native
//! promise drag; at drop time the OS calls [`DragFetch::fetch`] **on a background
//! thread** with the chosen destination. We turn that into an ordinary
//! [`Command::Download`] and block the callback until the transfer's terminal
//! [`Event::TransferDone`] arrives — so the drop streams through the existing
//! queue, path-locks, collision handling and progress dock for free.
//!
//! [`DragDownloads`] is the correlation seam: the UI event loop feeds it the
//! `TransferQueued`/`TransferDone` events (cheap no-ops for non-drag transfers),
//! and the off-thread [`ServiceDragFetch`] waits on the matching slot. Nothing
//! here touches GPUI entities, so it is safe off the main thread.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use nyx_core::{RemotePath, TransferId, TransferStatus};
use nyx_drag::{DragError, DragFetch, DragFile};
use nyx_service::{Command, CommandSender};

/// The completion slot for one in-flight drag-out download.
struct Slot {
    done: Mutex<Option<Result<(), String>>>,
    cv: Condvar,
}

impl Slot {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            done: Mutex::new(None),
            cv: Condvar::new(),
        })
    }

    /// Block until the transfer completes, returning its result.
    fn wait(&self) -> Result<(), String> {
        let mut done = self.done.lock().unwrap();
        while done.is_none() {
            done = self.cv.wait(done).unwrap();
        }
        done.take().unwrap()
    }

    fn complete(&self, result: Result<(), String>) {
        *self.done.lock().unwrap() = Some(result);
        self.cv.notify_all();
    }
}

#[derive(Default)]
struct Inner {
    /// Destination path (display form) → waiting slot.
    pending: HashMap<String, Arc<Slot>>,
    /// Transfer id → destination path, learned on `TransferQueued`.
    ids: HashMap<TransferId, String>,
}

/// Correlates drag-out promise downloads with their transfer events. Cloneable
/// and thread-safe: shared between the UI event loop and the off-thread fetch.
#[derive(Clone, Default)]
pub struct DragDownloads {
    inner: Arc<Mutex<Inner>>,
}

impl DragDownloads {
    pub fn new() -> Self {
        Self::default()
    }

    fn register(&self, dest: &str) -> Arc<Slot> {
        let slot = Slot::new();
        self.inner
            .lock()
            .unwrap()
            .pending
            .insert(dest.to_string(), slot.clone());
        slot
    }

    fn unregister(&self, dest: &str) {
        self.inner.lock().unwrap().pending.remove(dest);
    }

    /// Map a freshly-queued transfer to its drag-out slot, if it is one. Called
    /// from the UI event loop on [`Event::TransferQueued`](nyx_service::Event).
    pub fn note_queued(&self, id: TransferId, local: &str) {
        let mut inner = self.inner.lock().unwrap();
        if inner.pending.contains_key(local) {
            inner.ids.insert(id, local.to_string());
        }
    }

    /// Resolve a drag-out slot when its transfer reaches a terminal state.
    /// A no-op for non-drag transfers. Called on
    /// [`Event::TransferDone`](nyx_service::Event).
    pub fn note_done(&self, id: TransferId, status: TransferStatus, message: Option<&str>) {
        let mut inner = self.inner.lock().unwrap();
        let Some(dest) = inner.ids.remove(&id) else {
            return;
        };
        if let Some(slot) = inner.pending.remove(&dest) {
            let result = match status {
                TransferStatus::Completed => Ok(()),
                TransferStatus::Failed => Err(message.unwrap_or("download failed").to_string()),
                TransferStatus::Cancelled => Err("download cancelled".to_string()),
                TransferStatus::Skipped => Err("download skipped".to_string()),
                _ => Err("download did not complete".to_string()),
            };
            slot.complete(result);
        }
    }
}

/// The [`DragFetch`] that streams a dragged-out file or folder through the
/// download queue.
pub struct ServiceDragFetch {
    commands: CommandSender,
    downloads: DragDownloads,
    /// File name → `(remote source path, is_dir)`, for this drag.
    remotes: HashMap<String, (RemotePath, bool)>,
}

impl ServiceDragFetch {
    pub fn new(
        commands: CommandSender,
        downloads: DragDownloads,
        remotes: HashMap<String, (RemotePath, bool)>,
    ) -> Self {
        Self {
            commands,
            downloads,
            remotes,
        }
    }
}

impl DragFetch for ServiceDragFetch {
    fn fetch(&self, file: &DragFile, dest: &Path) -> Result<(), DragError> {
        let (remote, is_dir) = self
            .remotes
            .get(&file.name)
            .ok_or_else(|| DragError::fetch(format!("no remote source for “{}”", file.name)))?
            .clone();
        let key = dest.display().to_string();
        let slot = self.downloads.register(&key);
        if !self.commands.send(Command::Download {
            remote,
            local: dest.to_path_buf(),
            is_dir,
        }) {
            self.downloads.unregister(&key);
            return Err(DragError::fetch("backend unavailable"));
        }
        slot.wait().map_err(DragError::fetch)
    }
}
