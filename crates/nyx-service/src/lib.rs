//! The Nyx backend service.
//!
//! GPUI runs its own executor on the main thread; `russh` (the SFTP transport) is
//! Tokio-based. So the backend lives on a **dedicated thread** that owns a Tokio
//! runtime, the active connection and (later) the transfer queue. The UI talks to
//! it over two channels:
//!
//! - [`Command`] - UI → service (sent synchronously from the GPUI thread over a
//!   Tokio mpsc; a send never blocks the UI).
//! - [`Event`] - service → UI. This side is a `futures::channel::mpsc` so the
//!   GPUI **foreground** executor can `await` it as a `Stream` inside `cx.spawn`
//!   (a blocking `std` recv there would freeze the UI).
//!
//! A single connection is supported (the active session); multi-session is out
//! of scope. [`Command::TestConnection`] / [`Event::TestResult`] back the
//! connection editor's "Test" button: the probe spins up a *transient* client
//! that never touches the stored session. A single-flight guard makes this safe:
//! at most one connect-like op (Connect or TestConnection) is in flight at a
//! time, so there is never more than one pending host-key decision.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;

use futures::channel::mpsc::{
    unbounded as futures_unbounded, UnboundedReceiver as FuturesReceiver,
    UnboundedSender as FuturesSender,
};
use futures::stream::{FuturesUnordered, StreamExt};
use nyx_core::{
    is_safe_local_segment, CollisionChoice, EntryIssue, EntryKind, Filter, NyxError, Permissions,
    Protocol, RemoteEntry, RemotePath, Secret, ServerTrustKind, TransferDirection, TransferId,
    TransferKind, TransferReport, TransferStatus, LARGE_LISTING_WARN,
};
use nyx_profile::{AuthMethod, Profile};
use nyx_protocol::{
    Auth, DirWalk, FtpClient, FtpsClient, KnownHosts, RemoteClient, SftpClient, WalkItem,
};
use nyx_transfer::{CancelOutcome, Started, TransferQueue, TransferSpec};
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver as TokioReceiver, UnboundedSender as TokioSender,
};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

mod dto;
mod reconnect;
mod search;
mod transfer;
pub use dto::*;
pub(crate) use reconnect::*;
pub(crate) use search::*;
pub(crate) use transfer::*;

/// The global concurrency cap: at most this many transfers run at once;
/// submissions past it wait in the queue. Per-profile / settings caps are
/// post-MVP.
const MAX_CONCURRENT_TRANSFERS: usize = 3;

/// The transfer concurrency cap for a protocol. FTP/FTPS keep a single stateful
/// control connection and serialize every operation over one stream mutex, so
/// more than one in-flight transfer can't actually run - cap them at 1 to keep
/// the queue (and the dock) honest rather than showing stalled "running" slots.
/// SFTP multiplexes channels and gets the full cap.
fn transfer_cap_for(protocol: Protocol) -> usize {
    match protocol {
        Protocol::Sftp => MAX_CONCURRENT_TRANSFERS,
        Protocol::Ftp | Protocol::Ftps => 1,
    }
}

/// How often the dispatcher samples running transfers' byte counters to emit a
/// throttled [`Event::TransferProgress`]. The fixed interval also serves as the
/// speed denominator, so no `Instant` is needed.
const PROGRESS_TICK: Duration = Duration::from_millis(150);

/// Recursion depth cap for a tree search (the root is depth 0). Bounds work on
/// pathological trees and is the symlink-loop backstop.
const SEARCH_MAX_DEPTH: u32 = 32;

/// Hard cap on hits a single tree search returns. Beyond this the result is
/// marked truncated and the walk stops - keeps a wildcard query over a huge tree
/// from flooding the UI. Lift once `large-listings` lazifies the row precompute.
const SEARCH_MAX_RESULTS: usize = 5_000;

/// Hits buffered before a streaming [`Event::SearchResult`] batch is flushed, so
/// results appear as the tree is walked rather than all at the end.
const SEARCH_BATCH: usize = 128;

/// How many directory listings a tree search keeps in flight at once. A deep
/// search is round-trip bound, and SFTP pipelines requests over its single
/// connection, so concurrency cuts wall-clock time sharply. FTP serializes
/// internally, so it simply ignores the extra parallelism - no benefit, no harm.
const SEARCH_CONCURRENCY: usize = 16;

/// How many automatic reconnect attempts to make on a transport loss before
/// giving up and falling back to a manual reconnect.
const RECONNECT_MAX_ATTEMPTS: u32 = 6;

/// The exponential-backoff cap: the base wait doubles per attempt (1s, 2s, 4s …)
/// but never exceeds this, with jitter added on top to avoid reconnect storms.
const RECONNECT_CAP: Duration = Duration::from_secs(30);

/// The per-OS path to the trust-on-first-use `known_hosts` store
/// (`<data_dir>/known_hosts`, resolved via the `directories` crate).
///
/// Falls back to `./known_hosts` only if the OS data dir can't be resolved
/// (never expected in practice).
fn known_hosts() -> PathBuf {
    match directories::ProjectDirs::from("dev", "nyx", "Nyx") {
        Some(dirs) => dirs.data_dir().join("known_hosts"),
        None => {
            warn!("could not resolve the OS data directory; using ./known_hosts");
            PathBuf::from("known_hosts")
        }
    }
}

/// The per-OS path to the FTPS trust-on-first-use `known_certs` store
/// (`<data_dir>/known_certs`) - the certificate parallel to [`known_hosts`].
fn known_certs() -> PathBuf {
    match directories::ProjectDirs::from("dev", "nyx", "Nyx") {
        Some(dirs) => dirs.data_dir().join("known_certs"),
        None => {
            warn!("could not resolve the OS data directory; using ./known_certs");
            PathBuf::from("known_certs")
        }
    }
}

/// Handle to the running backend thread.
///
/// Send [`Command`]s with [`ServiceHandle::send`]. Dropping the handle requests
/// shutdown and joins the thread.
pub struct ServiceHandle {
    commands: TokioSender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl ServiceHandle {
    /// Send a command to the backend. Returns `false` if the service has gone.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    /// A cloneable, `Send + Sync` sender for the command channel.
    ///
    /// Lets off-thread code (e.g. an OS drag-promise callback) submit commands
    /// without borrowing the UI-thread [`ServiceHandle`]. It is the same channel
    /// [`send`](Self::send) uses, so ordering and shutdown semantics are shared.
    pub fn commands(&self) -> CommandSender {
        CommandSender {
            tx: self.commands.clone(),
        }
    }
}

/// A detached, thread-safe handle to the command channel (see
/// [`ServiceHandle::commands`]).
#[derive(Clone)]
pub struct CommandSender {
    tx: TokioSender<Command>,
}

impl CommandSender {
    /// Send a command to the backend. Returns `false` if the service has gone.
    pub fn send(&self, command: Command) -> bool {
        self.tx.send(command).is_ok()
    }
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Spawn the backend thread and its Tokio runtime.
///
/// Returns the [`ServiceHandle`] (UI → service) and the [`Event`] receiver
/// (service → UI) the UI drains as a `Stream` on the GPUI foreground executor.
pub fn spawn() -> (ServiceHandle, FuturesReceiver<Event>) {
    let (cmd_tx, cmd_rx) = unbounded_channel::<Command>();
    // Unbounded is deliberate and safe here: the only high-frequency producer is
    // the progress sampler, already coalesced to one `TransferProgress` per running
    // transfer per `PROGRESS_TICK` (≤ the concurrency cap, so a small constant
    // rate). Every other event is one-per-user-action or one-per-transfer-terminal
    // - naturally bounded by what the user did. So the queue can only grow if the
    // GPUI foreground executor stops draining entirely, which means the UI is
    // already wedged (a fatal condition, not backpressure to manage). Bounding here
    // would buy nothing but a risk of dropping a consequential terminal event.
    let (evt_tx, evt_rx) = futures_unbounded::<Event>();

    let thread = thread::Builder::new()
        .name("nyx-service".into())
        .spawn(move || run(cmd_rx, evt_tx))
        .expect("failed to spawn nyx-service thread");

    (
        ServiceHandle {
            commands: cmd_tx,
            thread: Some(thread),
        },
        evt_rx,
    )
}

/// The backend thread entry point: build the runtime and drive the dispatcher.
fn run(commands: TokioReceiver<Command>, events: FuturesSender<Event>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("nyx-service-worker")
        .build()
        .expect("failed to build Tokio runtime");

    let _ = events.unbounded_send(Event::Ready);
    runtime.block_on(dispatch(commands, events.clone()));
    let _ = events.unbounded_send(Event::Stopped);
}

/// Whether a connect-like task is a live connect or a throwaway test probe.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskKind {
    /// A live `Connect` - its session is kept on success.
    Connect,
    /// A `TestConnection` probe - the client is dropped after reporting.
    Test,
}

/// The result of a connect-like task, handed back to the dispatcher.
enum TaskOutcome {
    /// A live connect succeeded - the dispatcher takes ownership of the session.
    Connected {
        profile_id: String,
        /// The connected protocol, so the dispatcher can size the transfer
        /// concurrency cap (FTP/FTPS serialize over one connection → cap 1).
        protocol: Protocol,
        client: Box<dyn RemoteClient>,
        /// The resolved default landing directory (home).
        home: RemotePath,
    },
    /// A live connect failed with a credential-free message.
    ConnectFailed { message: String },
    /// An auto-reconnect loop gave up after exhausting its attempts or hitting a
    /// non-transport failure.
    ReconnectFailed { profile_id: String, reason: String },
    /// A test probe finished (success or failure).
    TestResult {
        profile_id: String,
        ok: bool,
        message: String,
    },
}

/// The single command dispatcher. Owns the active session and the host-key
/// decision slot; connect-like ops run as spawned tasks so the loop stays
/// responsive to [`Command::HostKeyDecision`] while a handshake awaits the user.
///
/// A single-flight guard (`in_flight`) covers both `Connect` and
/// `TestConnection`: while one is running, a second connect-like command is
/// rejected outright, so the single host-key slot can never be contended.
async fn dispatch(mut commands: TokioReceiver<Command>, events: FuturesSender<Event>) {
    // `Arc`-shared so slow ops (download/upload/remove) can clone a handle into a
    // detached task and run concurrently against the one session, without
    // blocking the command loop.
    let mut client: Option<Arc<dyn RemoteClient>> = None;
    // The id of the profile behind the active session, for the `ConnectionLost`
    // event. Set on connect, cleared on disconnect or a detected loss.
    let mut active_profile: Option<String> = None;
    // The responder for an in-flight host-key prompt. With the single-flight
    // guard there is at most one connect-like op, hence at most one slot user.
    let mut pending_host_key: Option<oneshot::Sender<bool>> = None;
    // Whether a connect-like op (Connect or TestConnection) is in flight.
    let mut in_flight = false;

    // The transfer scheduler (sans-IO policy) and the per-id byte counters from
    // the previous progress tick (for the speed delta).
    let mut queue = TransferQueue::new(MAX_CONCURRENT_TRANSFERS);
    let mut last_bytes: HashMap<TransferId, u64> = HashMap::new();

    // Internal channels: connect-like task → dispatcher.
    let (register_tx, mut register_rx) = unbounded_channel::<oneshot::Sender<bool>>();
    let (done_tx, mut done_rx) = unbounded_channel::<(u64, TaskOutcome)>();
    // Internal channel: a finished copy task → dispatcher (mirrors `done`). The
    // `u64` is the session generation the copy ran under (see `generation` below).
    let (xfer_done_tx, mut xfer_done_rx) =
        unbounded_channel::<(TransferId, u64, TransferOutcome)>();

    // Owns the session credentials cached for auto-reconnect and the backoff loop.
    let mut reconnector = Reconnector::new(register_tx.clone(), done_tx.clone());

    // Bumped on every successful connect. Each spawned copy task carries the
    // generation it ran under, so a straggler that only notices a drop *after* a
    // reconnect already succeeded can't be mistaken for a fresh loss of the new,
    // healthy session - it's resumed instead of flipping the session again.
    let mut generation: u64 = 0;

    // The in-flight tree search, if any: its cancel flag (stops the client walk
    // between listings) and its task handle (aborting drops a server-side `find`'s
    // channel, killing it). A new `SearchTree`/`CancelSearch`/connect/disconnect
    // ends it.
    let mut current_search: Option<(Arc<AtomicBool>, tokio::task::JoinHandle<()>)> = None;

    // The throttle ticker for progress sampling. The first tick fires
    // immediately; on an idle loop it samples an empty set (cheap no-op).
    let mut progress_tick = tokio::time::interval(PROGRESS_TICK);

    loop {
        tokio::select! {
            maybe_cmd = commands.recv() => {
                let Some(command) = maybe_cmd else { break };
                match command {
                    Command::Shutdown => break,
                    Command::Connect { profile, secret, auto_reconnect } => {
                        if in_flight {
                            let _ = events.unbounded_send(Event::Error {
                                message: "a connection is already in progress".into(),
                            });
                            continue;
                        }
                        // A manual connect supersedes any backoff loop. A manual
                        // *reconnect* (same profile) keeps interrupted transfers so
                        // they resume on `Connected`; connecting to a *different*
                        // profile cancels them - they belong to the old session.
                        reconnector.abort();
                        let same_profile = reconnector
                            .creds
                            .as_ref()
                            .map(|c| c.profile.id.as_str())
                            == Some(profile.id.as_str());
                        if !same_profile {
                            for id in queue.drain_interrupted() {
                                last_bytes.remove(&id);
                                let _ = events.unbounded_send(Event::TransferDone {
                                    id,
                                    status: TransferStatus::Cancelled,
                                    message: None,
                                    report: None,
                                });
                            }
                        }
                        reconnector.set_creds(profile.clone(), secret.clone(), auto_reconnect);
                        // Replace any existing session.
                        abort_search(&mut current_search);
                        client = None;
                        in_flight = true;
                        // Supersede any prior intent (incl. the loop just aborted).
                        let seq = reconnector.bump();
                        tokio::spawn(run_task(
                            TaskKind::Connect,
                            profile,
                            secret,
                            events.clone(),
                            register_tx.clone(),
                            done_tx.clone(),
                            seq,
                        ));
                    }
                    Command::TestConnection { profile, secret } => {
                        if in_flight {
                            let _ = events.unbounded_send(Event::TestResult {
                                profile_id: profile.id,
                                ok: false,
                                message: "a connection is already in progress".into(),
                            });
                            continue;
                        }
                        in_flight = true;
                        // A probe carries the current epoch but doesn't supersede an
                        // ongoing connect/auto-reconnect, so it never bumps.
                        let seq = reconnector.seq();
                        tokio::spawn(run_task(
                            TaskKind::Test,
                            profile,
                            secret,
                            events.clone(),
                            register_tx.clone(),
                            done_tx.clone(),
                            seq,
                        ));
                    }
                    Command::HostKeyDecision { accept } => {
                        if let Some(responder) = pending_host_key.take() {
                            let _ = responder.send(accept);
                        } else {
                            warn!("host-key decision with no pending prompt");
                        }
                    }
                    // The result is computed first so the immutable session borrow
                    // ends before a `ConnectionLost` error needs `&mut client`.
                    Command::ListDir { path } => {
                        let result = match client.as_deref() {
                            Some(session) => Some(session.list_dir(&path).await),
                            None => None,
                        };
                        match result {
                            Some(Ok(entries)) => {
                                let count = entries.len();
                                debug!(%path, count, "listed directory");
                                if count >= LARGE_LISTING_WARN {
                                    warn!(%path, count, "very large directory listing");
                                }
                                let _ = events.unbounded_send(Event::DirListing { path, entries });
                            }
                            Some(Err(err)) => report_op_error(
                                err, &mut client, &mut active_profile, &mut queue,
                                &mut last_bytes, &mut reconnector, &events,
                            ),
                            None => not_connected(&events),
                        }
                    }
                    Command::SearchTree { root, query, token } => {
                        // Supersede any in-flight search, then search off the command
                        // loop so further commands (incl. the next keystroke's
                        // search) keep flowing while the tree is traversed.
                        abort_search(&mut current_search);
                        match client.clone() {
                            Some(session) => {
                                let cancel = Arc::new(AtomicBool::new(false));
                                let events = events.clone();
                                let handle = tokio::spawn(run_tree_search(
                                    session,
                                    root,
                                    query,
                                    token,
                                    cancel.clone(),
                                    events,
                                ));
                                current_search = Some((cancel, handle));
                            }
                            None => not_connected(&events),
                        }
                    }
                    Command::CancelSearch => abort_search(&mut current_search),
                    Command::ResolveSymlink { path } => {
                        let result = match client.as_deref() {
                            Some(session) => Some(session.target_kind(&path).await),
                            None => None,
                        };
                        match result {
                            Some(Ok(kind)) => {
                                let _ = events.unbounded_send(Event::SymlinkResolved {
                                    path,
                                    is_dir: kind == EntryKind::Directory,
                                });
                            }
                            Some(Err(err)) => report_op_error(
                                err, &mut client, &mut active_profile, &mut queue,
                                &mut last_bytes, &mut reconnector, &events,
                            ),
                            None => not_connected(&events),
                        }
                    }
                    // Quick metadata ops: one SFTP round-trip, awaited inline.
                    Command::Mkdir { path } => {
                        let result = match client.as_deref() {
                            Some(session) => Some(session.mkdir(&path).await),
                            None => None,
                        };
                        match result {
                            Some(Ok(())) => {
                                let _ = events.unbounded_send(Event::FileOpDone {
                                    op: FileOp::Mkdir,
                                    message: format!("Created “{}”", base_name(&path)),
                                });
                            }
                            Some(Err(err)) => report_op_error(
                                err, &mut client, &mut active_profile, &mut queue,
                                &mut last_bytes, &mut reconnector, &events,
                            ),
                            None => not_connected(&events),
                        }
                    }
                    Command::Rename { from, to } => {
                        // Reject a rename that would race a live transfer on either
                        // endpoint (the path-lock policy).
                        if queue.is_remote_locked(&from) || queue.is_remote_locked(&to) {
                            let _ = events.unbounded_send(Event::Error {
                                message: format!("“{}” has a transfer in progress", base_name(&from)),
                            });
                        } else {
                            tracing::info!(from = %from.as_str(), to = %to.as_str(), "rename: issuing");
                            let result = match client.as_deref() {
                                Some(session) => Some(session.rename(&from, &to).await),
                                None => None,
                            };
                            match result {
                                Some(Ok(())) => {
                                    tracing::info!(to = %to.as_str(), "rename: ok");
                                    let _ = events.unbounded_send(Event::FileOpDone {
                                        op: FileOp::Rename,
                                        message: format!("Renamed to “{}”", base_name(&to)),
                                    });
                                }
                                Some(Err(err)) => {
                                    tracing::warn!(error = %err, "rename: failed");
                                    report_op_error(
                                        err, &mut client, &mut active_profile, &mut queue,
                                        &mut last_bytes, &mut reconnector, &events,
                                    )
                                }
                                None => not_connected(&events),
                            }
                        }
                    }
                    // Slow ops: spawned against a cloned `Arc` so the loop stays
                    // responsive (and several can run at once); each emits its own
                    // terminal event. A missing session is reported immediately.
                    Command::Remove { path, is_dir } => {
                        // Reject a delete that would race a live transfer on the
                        // same path (the path-lock policy).
                        if queue.is_remote_locked(&path) {
                            let _ = events.unbounded_send(Event::Error {
                                message: format!("“{}” has a transfer in progress", base_name(&path)),
                            });
                        } else {
                            match client.clone() {
                                Some(session) => {
                                    let message = format!("Deleted “{}”", base_name(&path));
                                    spawn_file_op(FileOp::Remove, message, events.clone(), async move {
                                        let _ = is_dir; // protocol re-stats
                                        session.remove(&path).await
                                    });
                                }
                                None => not_connected(&events),
                            }
                        }
                    }
                    // Transfers go through the queue: submit → announce → try to
                    // start (subject to the cap). The dock row is the feedback -
                    // no `FileOpDone` toast for transfers.
                    Command::Download { remote, local, is_dir } => {
                        submit_transfer(
                            &mut queue, &client, &events, &xfer_done_tx, generation,
                            TransferDirection::Download, kind_of(is_dir), remote, local,
                        );
                    }
                    Command::Upload { local, remote, is_dir } => {
                        submit_transfer(
                            &mut queue, &client, &events, &xfer_done_tx, generation,
                            TransferDirection::Upload, kind_of(is_dir), remote, local,
                        );
                    }
                    Command::CancelTransfer { id } => match queue.cancel(id) {
                        // Never started: no task will report, so emit the terminal
                        // Cancelled directly.
                        CancelOutcome::WasQueued => {
                            last_bytes.remove(&id);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id,
                                status: TransferStatus::Cancelled,
                                message: None,
                                report: None,
                            });
                        }
                        // Running: the copy loop notices the flag and reports
                        // through `xfer_done` on the normal terminal path.
                        CancelOutcome::WasRunning => {}
                        CancelOutcome::Unknown => {}
                    },
                    Command::ResolveCollision { id, choice, apply_to_all } => {
                        // Skip/Cancel resolutions terminate the parked items here
                        // (no task ran for them); Overwrite re-queues them, so a
                        // try_start picks them up.
                        let resolution = queue.resolve(id, choice, apply_to_all);
                        for sid in resolution.skipped {
                            last_bytes.remove(&sid);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id: sid,
                                status: TransferStatus::Skipped,
                                message: None,
                                report: None,
                            });
                        }
                        for cid in resolution.cancelled {
                            last_bytes.remove(&cid);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id: cid,
                                status: TransferStatus::Cancelled,
                                message: None,
                                report: None,
                            });
                        }
                        try_start(&mut queue, &client, &events, &xfer_done_tx, generation);
                    }
                    Command::CancelReconnect => {
                        // Stop the backoff loop but keep the session credentials -
                        // a later manual reconnect re-seeds them anyway. Bump the
                        // epoch so a `Connected` the loop already queued is dropped.
                        reconnector.bump();
                        reconnector.abort();
                    }
                    Command::Disconnect => {
                        // A disconnect also clears the single-flight slot, the
                        // active-profile tracking and any auto-reconnect state, and
                        // bumps the epoch so any in-flight connect outcome is dropped.
                        in_flight = false;
                        reconnector.bump();
                        active_profile = None;
                        reconnector.clear();
                        abort_search(&mut current_search);
                        // Cancel everything: flag the running transfers (their
                        // tasks wind down and report Cancelled via `xfer_done`)
                        // and drain the queued ones (no task ran, so emit their
                        // terminal Cancelled here) - then drop the session.
                        for id in queue.cancel_all() {
                            last_bytes.remove(&id);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id,
                                status: TransferStatus::Cancelled,
                                message: None,
                                report: None,
                            });
                        }
                        // Drop the shared session: its `Drop` closes the channel
                        // + connection. Any still-winding-down transfer task holds
                        // a clone that keeps the connection alive until its next
                        // cancel check ends the copy.
                        if client.take().is_some() {
                            info!("disconnected");
                        }
                    }
                }
            }
            Some(responder) = register_rx.recv() => {
                pending_host_key = Some(responder);
            }
            Some((seq, done)) = done_rx.recv() => {
                // Drop an outcome from a superseded attempt (a straggler whose epoch
                // the dispatcher has since bumped): it must not adopt its session,
                // bump the generation, or clear the single-flight slot.
                if seq != reconnector.seq() {
                    continue;
                }
                // A current outcome clears the single-flight slot.
                in_flight = false;
                match done {
                    TaskOutcome::Connected { profile_id, protocol, client: session, home } => {
                        // A connect (manual or via the backoff loop) landed; drop
                        // the loop's now-finished handle.
                        reconnector.abort();
                        client = Some(Arc::from(session));
                        active_profile = Some(profile_id.clone());
                        generation += 1;
                        // Size the concurrency cap to the protocol: FTP/FTPS
                        // serialize every op over one connection, so admitting more
                        // than one transfer only stalls them behind the stream lock.
                        queue.set_cap(transfer_cap_for(protocol));
                        info!(%profile_id, "connected");
                        let _ = events.unbounded_send(Event::Connected { profile_id, home });
                        // Resume any transfers interrupted by the loss, in their
                        // original order (a no-op on a first connect).
                        if queue.readmit_interrupted() > 0 {
                            try_start(&mut queue, &client, &events, &xfer_done_tx, generation);
                        }
                    }
                    TaskOutcome::ConnectFailed { message } => {
                        // A failed manual connect holds no session to reconnect to.
                        reconnector.clear();
                        let _ = events.unbounded_send(Event::Error { message });
                    }
                    TaskOutcome::ReconnectFailed { profile_id, reason } => {
                        reconnector.clear();
                        warn!(%profile_id, "auto-reconnect gave up");
                        let _ = events.unbounded_send(Event::ReconnectFailed { profile_id, reason });
                    }
                    TaskOutcome::TestResult { profile_id, ok, message } => {
                        let _ = events.unbounded_send(Event::TestResult { profile_id, ok, message });
                    }
                }
            }
            Some((id, gen, outcome)) = xfer_done_rx.recv() => {
                match outcome {
                    // The pre-flight gate found an existing destination: park the
                    // item and ask the UI. If there is no UI to answer (the event
                    // channel is closed), default to Skip - never silent overwrite.
                    TransferOutcome::Collision { existing_size } => {
                        if let Some(spec) = queue.park(id) {
                            let sent = events
                                .unbounded_send(Event::TransferCollision {
                                    id,
                                    direction: spec.direction,
                                    is_dir: spec.kind == TransferKind::Dir,
                                    remote: spec.remote.clone(),
                                    local: spec.local.display().to_string(),
                                    existing_size,
                                })
                                .is_ok();
                            if !sent {
                                for sid in queue.resolve(id, CollisionChoice::Skip, false).skipped {
                                    last_bytes.remove(&sid);
                                }
                            }
                        }
                    }
                    // The transport died mid-copy: park this transfer in the
                    // resumable holding state, then decide what the loss means.
                    TransferOutcome::Interrupted { transferred, source_meta } => {
                        if queue.interrupt(id, transferred, source_meta) {
                            last_bytes.remove(&id);
                            let _ = events.unbounded_send(Event::TransferInterrupted { id, transferred });
                        }
                        if gen == generation {
                            // First sign of loss for the *current* session (a sibling
                            // may have flipped it already - note_connection_lost is
                            // idempotent). Flips the session, interrupts the pending
                            // transfers and starts the backoff loop.
                            note_connection_lost(
                                &mut client, &mut active_profile, &mut queue,
                                &mut last_bytes, &mut reconnector, &events,
                                "connection lost".into(),
                            );
                        } else if client.is_some() {
                            // A straggler from an already-replaced session noticing
                            // the *old* drop after we reconnected - don't flip the
                            // healthy session; re-admit it so the trailing try_start
                            // resumes it on the new one.
                            queue.readmit_interrupted();
                        }
                    }
                    // A copy task finished: free its slot, drop its speed counter,
                    // announce the terminal state. `terminate` is state-checked and
                    // idempotent: a duplicate or out-of-order terminal (e.g. for an
                    // id since moved to the resumable interrupted state) returns
                    // `false` and we do nothing - no double unlock, no spurious
                    // event - so it can't free a lock the still-live transfer needs.
                    terminal => {
                        if !queue.terminate(id) {
                            continue;
                        }
                        last_bytes.remove(&id);
                        let resolved = match terminal {
                            TransferOutcome::Completed { message, report } => {
                                Some((TransferStatus::Completed, message, report))
                            }
                            TransferOutcome::Cancelled => Some((TransferStatus::Cancelled, None, None)),
                            TransferOutcome::Skipped => Some((TransferStatus::Skipped, None, None)),
                            TransferOutcome::Failed(msg) => Some((TransferStatus::Failed, Some(msg), None)),
                            // Collision and Interrupted are handled by the arms above
                            // and never fall through here. Ignore rather than panic the
                            // backend thread if that invariant is ever broken.
                            TransferOutcome::Collision { .. } | TransferOutcome::Interrupted { .. } => {
                                warn!(?id, "unexpected non-terminal transfer outcome in terminal arm");
                                None
                            }
                        };
                        if let Some((status, message, report)) = resolved {
                            let _ = events.unbounded_send(Event::TransferDone { id, status, message, report });
                        }
                    }
                }
                // Backfill any freed slot (a parked item frees its slot too).
                try_start(&mut queue, &client, &events, &xfer_done_tx, generation);
            }
            _ = progress_tick.tick() => {
                // Sample every running transfer's byte counter and emit a
                // throttled progress event with an instantaneous speed.
                let samples: Vec<(TransferId, u64)> = queue.running_progress().collect();
                for (id, transferred) in samples {
                    let last = last_bytes.get(&id).copied().unwrap_or(0);
                    let delta = transferred.saturating_sub(last);
                    let speed_bps = delta * 1000 / PROGRESS_TICK.as_millis() as u64;
                    last_bytes.insert(id, transferred);
                    let _ = events.unbounded_send(Event::TransferProgress {
                        id,
                        transferred,
                        speed_bps,
                    });
                }
            }
        }
    }
}

/// Emit the standard "not connected" error (a file op arrived with no session).
fn not_connected(events: &FuturesSender<Event>) {
    let _ = events.unbounded_send(Event::Error {
        message: "not connected".into(),
    });
}

/// Emit the path-lock rejection for a transfer whose path already has a live one.
fn path_in_use(events: &FuturesSender<Event>, remote: &RemotePath) {
    let _ = events.unbounded_send(Event::Error {
        message: format!("“{}” already has a transfer in progress", base_name(remote)),
    });
}

/// Report an inline op's error: a transport death flips the session to "lost"
/// (via [`note_connection_lost`]); anything else is a plain [`Event::Error`].
fn report_op_error(
    err: NyxError,
    client: &mut Option<Arc<dyn RemoteClient>>,
    active_profile: &mut Option<String>,
    queue: &mut TransferQueue,
    last_bytes: &mut HashMap<TransferId, u64>,
    reconnector: &mut Reconnector,
    events: &FuturesSender<Event>,
) {
    if matches!(err, NyxError::ConnectionLost(_)) {
        note_connection_lost(
            client,
            active_profile,
            queue,
            last_bytes,
            reconnector,
            events,
            err.to_string(),
        );
    } else {
        let _ = events.unbounded_send(Event::Error {
            message: err.to_string(),
        });
    }
}

/// Flip the active session to "lost": drop the client (so later commands fail
/// fast), emit exactly one [`Event::ConnectionLost`], move the **pending**
/// transfers (queued/parked) into the resumable interrupted state, and kick off
/// an auto-reconnect backoff loop (a no-op when the setting is off or no
/// credentials are cached). In-flight transfers interrupt themselves as their
/// copy tasks notice the drop. The `client.take()` guard makes this idempotent -
/// a later op that also sees a transport error finds no client and is a no-op.
fn note_connection_lost(
    client: &mut Option<Arc<dyn RemoteClient>>,
    active_profile: &mut Option<String>,
    queue: &mut TransferQueue,
    last_bytes: &mut HashMap<TransferId, u64>,
    reconnector: &mut Reconnector,
    events: &FuturesSender<Event>,
    reason: String,
) {
    if client.take().is_none() {
        return;
    }
    let profile_id = active_profile.take().unwrap_or_default();
    warn!(%profile_id, "connection lost");
    let _ = events.unbounded_send(Event::ConnectionLost { profile_id, reason });
    for id in queue.interrupt_pending() {
        last_bytes.remove(&id);
        let _ = events.unbounded_send(Event::TransferInterrupted { id, transferred: 0 });
    }
    reconnector.start(events);
}

/// The path's final component (the file/folder name) for toast copy, falling
/// back to `/` at the root. Paths are not secrets, so this is safe to surface.
fn base_name(path: &RemotePath) -> &str {
    path.file_name().unwrap_or("/")
}

/// Spawn a slow file op as a detached task: run `fut` against the cloned session,
/// then emit [`Event::FileOpDone`] on success or [`Event::Error`] on failure.
///
/// The op never touches the dispatcher's session slot or single-flight guard, so
/// several can run concurrently (`russh-sftp` multiplexes over the one channel).
fn spawn_file_op<F>(op: FileOp, message: String, events: FuturesSender<Event>, fut: F)
where
    F: Future<Output = nyx_core::Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        let event = match fut.await {
            Ok(()) => Event::FileOpDone { op, message },
            Err(err) => Event::Error {
                message: err.to_string(),
            },
        };
        let _ = events.unbounded_send(event);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_core::Protocol;

    #[test]
    fn connect_debug_redacts_the_secret() {
        let cmd = Command::Connect {
            profile: Profile {
                id: "id".into(),
                name: "n".into(),
                protocol: Protocol::Sftp,
                ftps_mode: Default::default(),
                host: "example.com".into(),
                port: 22,
                username: "user".into(),
                auth: AuthMethod::Password,
                remote_path: None,
                color: Default::default(),
                last_connected: None,
            },
            secret: Secret::new("hunter2"),
            auto_reconnect: true,
        };
        let dbg = format!("{cmd:?}");
        assert!(dbg.contains("***"), "{dbg}");
        assert!(!dbg.contains("hunter2"), "{dbg}");
    }

    fn profile_with(protocol: Protocol, auth: AuthMethod) -> Profile {
        Profile {
            id: "id".into(),
            name: "n".into(),
            protocol,
            ftps_mode: Default::default(),
            host: "example.com".into(),
            port: 21,
            username: "user".into(),
            auth,
            remote_path: None,
            color: Default::default(),
            last_connected: None,
        }
    }

    #[test]
    fn anonymous_ftp_login_ignores_username_and_any_secret() {
        let profile = profile_with(Protocol::Ftp, AuthMethod::Anonymous);
        let (user, pass) = ftp_credentials(&profile, &Secret::new("ignored"));
        assert_eq!(user, "anonymous");
        assert_eq!(pass, ANON_PASSWORD);
    }

    #[test]
    fn password_ftp_login_uses_profile_username_and_secret() {
        let profile = profile_with(Protocol::Ftp, AuthMethod::Password);
        let (user, pass) = ftp_credentials(&profile, &Secret::new("hunter2"));
        assert_eq!(user, "user");
        assert_eq!(pass, "hunter2");
    }

    #[test]
    fn anonymous_is_rejected_for_sftp() {
        let (events_tx, _events_rx) = futures::channel::mpsc::unbounded();
        let (register_tx, _register_rx) = unbounded_channel();
        let prompt = Arc::new(host_key::PromptBridge {
            events: events_tx,
            register: register_tx,
        });
        let profile = profile_with(Protocol::Sftp, AuthMethod::Anonymous);
        assert!(build_client(&profile, Secret::new(""), prompt).is_err());
    }

    #[test]
    fn backoff_grows_then_caps() {
        // Each attempt's delay (base, before jitter) doubles, then caps at 30s.
        // Jitter is at most +50% of the base, so an upper bound per attempt holds.
        for (attempt, base) in [(1u32, 1u64), (2, 2), (3, 4), (4, 8), (5, 16), (6, 30)] {
            let d = backoff_delay(attempt).as_millis() as u64;
            assert!(d >= base * 1000, "attempt {attempt}: {d} < {}", base * 1000);
            assert!(
                d <= base * 1000 + base * 500,
                "attempt {attempt}: {d} exceeds base+jitter"
            );
        }
        // Past the cap the base stays at 30s.
        assert!(backoff_delay(9).as_millis() as u64 <= 45_000);
    }

    #[test]
    fn only_transport_errors_are_retried() {
        assert!(is_transient_connect_error(&NyxError::Connection(
            "x".into()
        )));
        assert!(is_transient_connect_error(&NyxError::ConnectionLost(
            "x".into()
        )));
        assert!(is_transient_connect_error(&NyxError::Io("x".into())));
        // Credential / trust failures must not be retried.
        assert!(!is_transient_connect_error(&NyxError::Auth));
        assert!(!is_transient_connect_error(&NyxError::HostKey("x".into())));
        assert!(!is_transient_connect_error(&NyxError::KeyLocked));
    }

    #[test]
    fn part_paths_are_siblings_keeping_the_full_name() {
        // The temp sits in the same directory (same volume → atomic rename) and
        // appends - never replaces - the extension, so `foo.txt` stays distinct
        // from a sibling `foo.tar`.
        assert_eq!(
            local_part_path(Path::new("/home/u/foo.txt")),
            PathBuf::from("/home/u/foo.txt.nyxpart")
        );
        assert_eq!(
            remote_part_path(&RemotePath::new("/srv/data/foo.txt")),
            RemotePath::new("/srv/data/foo.txt.nyxpart")
        );
    }

    #[test]
    fn ftp_protocols_cap_transfers_at_one() {
        // FTP/FTPS serialize over one connection → cap 1; SFTP gets the full cap.
        assert_eq!(transfer_cap_for(Protocol::Sftp), MAX_CONCURRENT_TRANSFERS);
        assert_eq!(transfer_cap_for(Protocol::Ftp), 1);
        assert_eq!(transfer_cap_for(Protocol::Ftps), 1);
    }

    #[test]
    fn stat_error_counts_as_present_not_absent() {
        // A definite "absent" is the only result that skips the collision gate.
        assert!(!treat_as_present::<std::io::Error>(Ok(false)));
        assert!(treat_as_present::<std::io::Error>(Ok(true)));
        // An error must NOT degrade to "absent" → blind overwrite.
        assert!(treat_as_present(Err::<bool, _>(std::io::Error::other("x"))));
    }

    #[test]
    fn may_overwrite_only_on_resolved_overwrite() {
        assert!(may_overwrite(&resume_spec(0, None)));
        let mut spec = resume_spec(0, None);
        spec.on_collision = None;
        assert!(!may_overwrite(&spec));
        spec.on_collision = Some(CollisionChoice::Skip);
        assert!(!may_overwrite(&spec));
    }

    fn resume_spec(resume_from: u64, source_meta: Option<nyx_core::SourceMeta>) -> TransferSpec {
        TransferSpec {
            direction: TransferDirection::Download,
            kind: TransferKind::File,
            remote: RemotePath::new("/r/f"),
            local: std::path::PathBuf::from("/l/f"),
            on_collision: Some(CollisionChoice::Overwrite),
            resume_from,
            source_meta,
        }
    }

    #[test]
    fn resume_only_when_supported_and_source_unchanged() {
        let meta = nyx_core::SourceMeta {
            size: 1000,
            mtime: Some(42),
        };
        // Fresh transfer (offset 0) never resumes.
        assert_eq!(
            resume_offset(true, &resume_spec(0, Some(meta)), Some(meta), Some(256)),
            0
        );
        // A resume-capable client with an unchanged source resumes from the
        // destination's actual size (256), not the recorded watermark (300).
        assert_eq!(
            resume_offset(true, &resume_spec(300, Some(meta)), Some(meta), Some(256)),
            256
        );
        // A non-resume-capable client always restarts.
        assert_eq!(
            resume_offset(false, &resume_spec(300, Some(meta)), Some(meta), Some(256)),
            0
        );
    }

    #[test]
    fn resume_restarts_on_any_doubt() {
        let orig = nyx_core::SourceMeta {
            size: 1000,
            mtime: Some(42),
        };
        // Size changed under us → restart.
        let bigger = nyx_core::SourceMeta {
            size: 2000,
            mtime: Some(42),
        };
        assert_eq!(
            resume_offset(true, &resume_spec(256, Some(orig)), Some(bigger), Some(256)),
            0
        );
        // mtime changed → restart.
        let touched = nyx_core::SourceMeta {
            size: 1000,
            mtime: Some(99),
        };
        assert_eq!(
            resume_offset(
                true,
                &resume_spec(256, Some(orig)),
                Some(touched),
                Some(256)
            ),
            0
        );
        // Source unstattable now → restart.
        assert_eq!(
            resume_offset(true, &resume_spec(256, Some(orig)), None, Some(256)),
            0
        );
        // No fingerprint captured at start → restart.
        assert_eq!(
            resume_offset(true, &resume_spec(256, None), Some(orig), Some(256)),
            0
        );
        // Destination unstattable → restart.
        assert_eq!(
            resume_offset(true, &resume_spec(256, Some(orig)), Some(orig), None),
            0
        );
        // mtime unknown on both sides (equal but unverifiable) → restart.
        let no_mtime = nyx_core::SourceMeta {
            size: 1000,
            mtime: None,
        };
        assert_eq!(
            resume_offset(
                true,
                &resume_spec(256, Some(no_mtime)),
                Some(no_mtime),
                Some(256)
            ),
            0
        );
        // Partial larger than the current source (truncated/replaced) → restart.
        assert_eq!(
            resume_offset(true, &resume_spec(256, Some(orig)), Some(orig), Some(1200)),
            0
        );
    }

    use nyx_core::Permissions;

    /// In-memory directory tree for [`run_search`] tests. A path absent from the
    /// map lists as an error - modeling an unreadable (skipped) directory.
    struct FakeTree {
        dirs: HashMap<String, Vec<RemoteEntry>>,
    }

    #[async_trait]
    impl DirLister for FakeTree {
        async fn list(&self, path: &RemotePath) -> nyx_core::Result<Vec<RemoteEntry>> {
            self.dirs
                .get(path.as_str())
                .cloned()
                .ok_or_else(|| NyxError::NotFound(path.as_str().into()))
        }
    }

    fn dir_entry(name: &str) -> RemoteEntry {
        RemoteEntry {
            name: name.into(),
            size: 0,
            kind: EntryKind::Directory,
            modified: None,
            permissions: Permissions::from_mode(0o755),
        }
    }

    fn file_entry(name: &str) -> RemoteEntry {
        RemoteEntry {
            name: name.into(),
            size: 0,
            kind: EntryKind::File,
            modified: None,
            permissions: Permissions::from_mode(0o644),
        }
    }

    /// Run a search to completion and gather all streamed hits.
    async fn collect_search(
        tree: &FakeTree,
        query: &str,
        cancel: Arc<AtomicBool>,
    ) -> (Vec<SearchHit>, bool, bool) {
        let (tx, mut rx) = futures_unbounded();
        run_search(
            tree,
            RemotePath::root(),
            Filter::parse(query),
            9,
            cancel,
            tx,
        )
        .await;
        // `run_search` owns and drops the sender on return, so the receiver is now
        // closed and drains the buffered batches.
        let (mut hits, mut done, mut truncated) = (Vec::new(), false, false);
        while let Some(event) = rx.next().await {
            if let Event::SearchResult {
                token,
                hits: batch,
                done: d,
                truncated: t,
            } = event
            {
                assert_eq!(token, 9);
                hits.extend(batch);
                done |= d;
                truncated |= t;
            }
        }
        (hits, done, truncated)
    }

    fn sample_tree() -> FakeTree {
        let mut dirs = HashMap::new();
        dirs.insert(
            "/".to_string(),
            vec![
                dir_entry("src"),
                file_entry("readme.md"),
                dir_entry("denied"),
            ],
        );
        dirs.insert(
            "/src".to_string(),
            vec![
                file_entry("main.rs"),
                file_entry("lib.rs"),
                dir_entry("deep"),
            ],
        );
        dirs.insert("/src/deep".to_string(), vec![file_entry("mod.rs")]);
        // "/denied" is intentionally absent → lists as an error (skipped).
        FakeTree { dirs }
    }

    #[tokio::test]
    async fn search_walks_subtree_and_matches() {
        let (hits, done, truncated) =
            collect_search(&sample_tree(), "*.rs", Arc::new(AtomicBool::new(false))).await;
        let mut paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, ["/src/deep/mod.rs", "/src/lib.rs", "/src/main.rs"]);
        assert!(done);
        assert!(!truncated);
    }

    #[tokio::test]
    async fn search_skips_unreadable_dirs_and_completes() {
        // The walk descends into the absent "/denied" dir, which errors; it must
        // be skipped rather than aborting the whole search.
        let (hits, done, _) = collect_search(
            &sample_tree(),
            "type:file",
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        assert!(done);
        assert_eq!(hits.len(), 4); // readme.md, main.rs, lib.rs, mod.rs
    }

    #[tokio::test]
    async fn search_respects_result_cap() {
        let many: Vec<RemoteEntry> = (0..SEARCH_MAX_RESULTS + 5)
            .map(|i| file_entry(&format!("f{i}.txt")))
            .collect();
        let mut dirs = HashMap::new();
        dirs.insert("/".to_string(), many);
        let tree = FakeTree { dirs };

        let (hits, done, truncated) =
            collect_search(&tree, "*.txt", Arc::new(AtomicBool::new(false))).await;
        assert_eq!(hits.len(), SEARCH_MAX_RESULTS);
        assert!(truncated);
        assert!(done);
    }

    #[tokio::test]
    async fn cancelled_search_emits_nothing() {
        let cancel = Arc::new(AtomicBool::new(true));
        let (hits, done, _) = collect_search(&sample_tree(), "*.rs", cancel).await;
        assert!(hits.is_empty());
        assert!(!done, "a superseded search owes no terminal batch");
    }
}
