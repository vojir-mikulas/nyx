// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The Nyx backend service.
//!
//! GPUI runs its own executor on the main thread; `russh` (the SFTP transport) is
//! Tokio-based. So the backend lives on a **dedicated thread** that owns a Tokio
//! runtime, the active connection and (later) the transfer queue. The UI talks to
//! it over two channels:
//!
//! - [`Command`] — UI → service (sent synchronously from the GPUI thread over a
//!   Tokio mpsc; a send never blocks the UI).
//! - [`Event`] — service → UI. This side is a `futures::channel::mpsc` so the
//!   GPUI **foreground** executor can `await` it as a `Stream` inside `cx.spawn`
//!   (a blocking `std` recv there would freeze the UI).
//!
//! M2 wires the connect + list vertical slice: [`Command::Connect`] /
//! [`Command::ListDir`] / [`Command::Disconnect`], host-key trust-on-first-use via
//! [`Command::HostKeyDecision`], and the matching events. A single connection is
//! supported (the active session); multi-session is out of scope until later.
//!
//! M3 adds exactly one command/event pair — [`Command::TestConnection`] /
//! [`Event::TestResult`] — for the connection editor's "Test" button. The probe
//! spins up a *transient* client that never touches the stored session. A
//! single-flight guard makes this safe: at most one connect-like op (Connect or
//! TestConnection) is in flight at a time, so there is never more than one
//! pending host-key decision.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures::channel::mpsc::{
    unbounded as futures_unbounded, UnboundedReceiver as FuturesReceiver,
    UnboundedSender as FuturesSender,
};
use nyx_core::{NyxError, RemoteEntry, TransferDirection, TransferId, TransferStatus};
use nyx_profile::Profile;
use nyx_protocol::{KnownHosts, RemoteClient, SftpClient};
use nyx_transfer::{CancelOutcome, Started, TransferQueue, TransferSpec};
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver as TokioReceiver, UnboundedSender as TokioSender,
};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

/// The global concurrency cap: at most this many transfers run at once;
/// submissions past it wait in the queue (plan M5, D2). Per-profile / settings
/// caps are post-MVP.
const MAX_CONCURRENT_TRANSFERS: usize = 3;

/// How often the dispatcher samples running transfers' byte counters to emit a
/// throttled [`Event::TransferProgress`] (plan M5, D12). The fixed interval also
/// serves as the speed denominator, so no `Instant` is needed.
const PROGRESS_TICK: Duration = Duration::from_millis(150);

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

/// A password that never reveals itself in `Debug`/logs.
///
/// The inner string is only reachable via [`Secret::expose`], which is called in
/// exactly one place (the SFTP auth call). Everything else — including the derived
/// `Debug` on [`Command`] — sees `***`.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wrap a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reveal the secret. Call sites must not log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// A request from the UI to the backend.
#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    /// Connect to `profile`, authenticating with `password`.
    ///
    /// The password is wrapped in [`Secret`] so it can never reach a log.
    Connect {
        /// The profile to connect to.
        profile: Profile,
        /// The login password (redacted in `Debug`).
        password: Secret,
    },
    /// The user's answer to a pending [`Event::HostKeyPrompt`].
    HostKeyDecision {
        /// `true` to trust (and persist) the host key, `false` to abort.
        accept: bool,
    },
    /// List a remote directory on the active connection.
    ListDir {
        /// Absolute remote path to list.
        path: String,
    },
    /// Create a remote directory on the active connection.
    Mkdir {
        /// Absolute remote path of the new directory.
        path: String,
    },
    /// Rename / move a remote entry on the active connection.
    Rename {
        /// Current absolute remote path.
        from: String,
        /// New absolute remote path.
        to: String,
    },
    /// Delete a remote entry on the active connection.
    ///
    /// `is_dir` lets the protocol pick a file delete vs. a recursive directory
    /// delete without an extra stat round-trip on the UI's behalf.
    Remove {
        /// Absolute remote path to delete.
        path: String,
        /// Whether the target is a directory (recursive delete).
        is_dir: bool,
    },
    /// Download a remote file to a chosen local path.
    Download {
        /// Absolute remote path to read.
        remote: String,
        /// Local destination chosen by the user.
        local: PathBuf,
    },
    /// Upload a local file to a remote path in the active connection's cwd.
    Upload {
        /// Local source path chosen by the user.
        local: PathBuf,
        /// Absolute remote destination path.
        remote: String,
    },
    /// Validate a profile's credentials without opening a browser session.
    ///
    /// Spins up a throwaway client (its own connect + drop), entirely separate
    /// from the stored session, and reports back via [`Event::TestResult`]. The
    /// password is wrapped in [`Secret`] so it can never reach a log.
    TestConnection {
        /// The profile to probe.
        profile: Profile,
        /// The login password (redacted in `Debug`).
        password: Secret,
    },
    /// Cancel a queued or running transfer by id (plan M5, D7).
    ///
    /// A queued transfer is dropped before it starts; a running one is stopped
    /// mid-flight between chunks. Either way the UI receives a terminal
    /// [`Event::TransferDone`] with `Cancelled`.
    CancelTransfer {
        /// The transfer to cancel.
        id: TransferId,
    },
    /// Close the active connection.
    Disconnect,
    /// Shut the runtime down and exit the thread.
    Shutdown,
}

/// A message from the backend to the UI.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// The backend thread and Tokio runtime are up.
    Ready,
    /// The backend has stopped (after [`Command::Shutdown`] or channel drop).
    Stopped,
    /// A connection attempt has started for `profile_id`.
    Connecting {
        /// The connecting profile's id.
        profile_id: String,
    },
    /// An unknown host key needs the user's trust decision (TOFU).
    ///
    /// The UI shows a prompt and replies with [`Command::HostKeyDecision`].
    HostKeyPrompt {
        /// The host the key belongs to.
        host: String,
        /// The SHA-256 fingerprint, e.g. `SHA256:…`.
        fingerprint: String,
    },
    /// The active connection is established for `profile_id`.
    Connected {
        /// The connected profile's id.
        profile_id: String,
    },
    /// A directory listing for `path` on the active connection.
    DirListing {
        /// The path that was listed (echoed so the UI can drop stale listings).
        path: String,
        /// The entries in that directory.
        entries: Vec<RemoteEntry>,
    },
    /// The outcome of a [`Command::TestConnection`] probe, matched by `profile_id`.
    ///
    /// The `message` is human-readable and credential-free (e.g. `"Connection
    /// OK"` or an error detail).
    TestResult {
        /// The probed profile's id (so the editor can match its inline status).
        profile_id: String,
        /// Whether the probe succeeded.
        ok: bool,
        /// A credential-free status / error message.
        message: String,
    },
    /// A file operation completed successfully on the active connection.
    ///
    /// `op` tells the UI whether to refresh the current listing (mutating ops do,
    /// a download does not); `message` is a ready-to-toast success line.
    FileOpDone {
        /// Which operation completed (drives the refresh decision).
        op: FileOp,
        /// A credential-free, human-readable success message.
        message: String,
    },
    /// A transfer was accepted into the queue (plan M5, D5). The UI creates a
    /// `Queued` dock row; paths are not secrets, so they are safe to carry.
    TransferQueued {
        /// The assigned transfer id.
        id: TransferId,
        /// Upload or download.
        direction: TransferDirection,
        /// The remote-side path.
        remote: String,
        /// The local-side path (display form).
        local: String,
    },
    /// A transfer left the queue and is now running. `total` is the size statted
    /// at start (`None` if it could not be determined).
    TransferStarted {
        /// The transfer id.
        id: TransferId,
        /// Total size in bytes, if known.
        total: Option<u64>,
    },
    /// A throttled progress sample for a running transfer (~every 150 ms).
    TransferProgress {
        /// The transfer id.
        id: TransferId,
        /// Cumulative bytes transferred so far.
        transferred: u64,
        /// Instantaneous speed in bytes/sec over the last sample interval.
        speed_bps: u64,
    },
    /// A transfer reached a terminal state: `Completed`, `Failed` or `Cancelled`.
    /// `message` carries the credential-free error detail for `Failed`.
    TransferDone {
        /// The transfer id.
        id: TransferId,
        /// The terminal status.
        status: TransferStatus,
        /// An error detail for `Failed`; `None` otherwise.
        message: Option<String>,
    },
    /// An operation failed. The message is human-readable and credential-free.
    Error {
        /// The error detail (a `NyxError` display; never contains a secret).
        message: String,
    },
}

/// Which file operation a [`Event::FileOpDone`] refers to.
///
/// The UI's only per-op divergence is whether to refresh the current listing:
/// the mutating ops do; `Download` leaves the remote unchanged and only toasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOp {
    /// A directory was created.
    Mkdir,
    /// An entry was renamed / moved.
    Rename,
    /// An entry (file or recursive directory) was deleted.
    Remove,
    /// A local file was uploaded to the remote.
    Upload,
    /// A remote file was downloaded to disk.
    Download,
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
    /// A live `Connect` — its session is kept on success.
    Connect,
    /// A `TestConnection` probe — the client is dropped after reporting.
    Test,
}

/// The result of a connect-like task, handed back to the dispatcher.
enum TaskOutcome {
    /// A live connect succeeded — the dispatcher takes ownership of the session.
    Connected {
        profile_id: String,
        client: Box<SftpClient>,
    },
    /// A live connect failed with a credential-free message.
    ConnectFailed { message: String },
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
    // blocking the command loop (plan M4 D1). M5 swaps the bare spawn for the
    // transfer queue, keeping this shared-session shape.
    let mut client: Option<Arc<SftpClient>> = None;
    // The responder for an in-flight host-key prompt. With the single-flight
    // guard there is at most one connect-like op, hence at most one slot user.
    let mut pending_host_key: Option<oneshot::Sender<bool>> = None;
    // Whether a connect-like op (Connect or TestConnection) is in flight.
    let mut in_flight = false;

    // The transfer scheduler (sans-IO policy, plan M5 D1/D9) and the per-id
    // byte counters from the previous progress tick (for the speed delta, D12).
    let mut queue = TransferQueue::new(MAX_CONCURRENT_TRANSFERS);
    let mut last_bytes: HashMap<TransferId, u64> = HashMap::new();

    // Internal channels: connect-like task → dispatcher.
    let (register_tx, mut register_rx) = unbounded_channel::<oneshot::Sender<bool>>();
    let (done_tx, mut done_rx) = unbounded_channel::<TaskOutcome>();
    // Internal channel: a finished copy task → dispatcher (mirrors `done`).
    let (xfer_done_tx, mut xfer_done_rx) = unbounded_channel::<(TransferId, TransferOutcome)>();

    // The throttle ticker for progress sampling. The first tick fires
    // immediately; on an idle loop it samples an empty set (cheap no-op).
    let mut progress_tick = tokio::time::interval(PROGRESS_TICK);

    loop {
        tokio::select! {
            maybe_cmd = commands.recv() => {
                let Some(command) = maybe_cmd else { break };
                match command {
                    Command::Shutdown => break,
                    Command::Connect { profile, password } => {
                        if in_flight {
                            let _ = events.unbounded_send(Event::Error {
                                message: "a connection is already in progress".into(),
                            });
                            continue;
                        }
                        // Replace any existing session.
                        client = None;
                        in_flight = true;
                        tokio::spawn(run_task(
                            TaskKind::Connect,
                            profile,
                            password,
                            events.clone(),
                            register_tx.clone(),
                            done_tx.clone(),
                        ));
                    }
                    Command::TestConnection { profile, password } => {
                        if in_flight {
                            let _ = events.unbounded_send(Event::TestResult {
                                profile_id: profile.id,
                                ok: false,
                                message: "a connection is already in progress".into(),
                            });
                            continue;
                        }
                        in_flight = true;
                        tokio::spawn(run_task(
                            TaskKind::Test,
                            profile,
                            password,
                            events.clone(),
                            register_tx.clone(),
                            done_tx.clone(),
                        ));
                    }
                    Command::HostKeyDecision { accept } => {
                        if let Some(responder) = pending_host_key.take() {
                            let _ = responder.send(accept);
                        } else {
                            warn!("host-key decision with no pending prompt");
                        }
                    }
                    Command::ListDir { path } => {
                        match client.as_deref() {
                            Some(session) => match session.list_dir(&path).await {
                                Ok(entries) => {
                                    debug!(%path, count = entries.len(), "listed directory");
                                    let _ = events.unbounded_send(Event::DirListing { path, entries });
                                }
                                Err(err) => {
                                    let _ = events.unbounded_send(Event::Error {
                                        message: err.to_string(),
                                    });
                                }
                            },
                            None => {
                                let _ = events.unbounded_send(Event::Error {
                                    message: "not connected".into(),
                                });
                            }
                        }
                    }
                    // Quick metadata ops: one SFTP round-trip, awaited inline.
                    Command::Mkdir { path } => match client.as_deref() {
                        Some(session) => {
                            let event = match session.mkdir(&path).await {
                                Ok(()) => Event::FileOpDone {
                                    op: FileOp::Mkdir,
                                    message: format!("Created “{}”", base_name(&path)),
                                },
                                Err(err) => Event::Error { message: err.to_string() },
                            };
                            let _ = events.unbounded_send(event);
                        }
                        None => not_connected(&events),
                    },
                    Command::Rename { from, to } => match client.as_deref() {
                        Some(session) => {
                            let event = match session.rename(&from, &to).await {
                                Ok(()) => Event::FileOpDone {
                                    op: FileOp::Rename,
                                    message: format!("Renamed to “{}”", base_name(&to)),
                                },
                                Err(err) => Event::Error { message: err.to_string() },
                            };
                            let _ = events.unbounded_send(event);
                        }
                        None => not_connected(&events),
                    },
                    // Slow ops: spawned against a cloned `Arc` so the loop stays
                    // responsive (and several can run at once); each emits its own
                    // terminal event. A missing session is reported immediately.
                    Command::Remove { path, is_dir } => match client.clone() {
                        Some(session) => {
                            let message = format!("Deleted “{}”", base_name(&path));
                            spawn_file_op(FileOp::Remove, message, events.clone(), async move {
                                let _ = is_dir; // protocol re-stats; kept for M5 queue
                                session.remove(&path).await
                            });
                        }
                        None => not_connected(&events),
                    },
                    // Transfers go through the queue: submit → announce → try to
                    // start (subject to the cap). The dock row is the feedback —
                    // no `FileOpDone` toast for transfers anymore (plan M5 D5/D6).
                    Command::Download { remote, local } => {
                        if client.is_none() {
                            not_connected(&events);
                        } else {
                            let spec = TransferSpec {
                                direction: TransferDirection::Download,
                                remote: remote.clone(),
                                local: local.clone(),
                            };
                            let id = queue.submit(spec);
                            let _ = events.unbounded_send(Event::TransferQueued {
                                id,
                                direction: TransferDirection::Download,
                                remote,
                                local: local.display().to_string(),
                            });
                            try_start(&mut queue, &client, &events, &xfer_done_tx);
                        }
                    }
                    Command::Upload { local, remote } => {
                        if client.is_none() {
                            not_connected(&events);
                        } else {
                            let spec = TransferSpec {
                                direction: TransferDirection::Upload,
                                remote: remote.clone(),
                                local: local.clone(),
                            };
                            let id = queue.submit(spec);
                            let _ = events.unbounded_send(Event::TransferQueued {
                                id,
                                direction: TransferDirection::Upload,
                                remote,
                                local: local.display().to_string(),
                            });
                            try_start(&mut queue, &client, &events, &xfer_done_tx);
                        }
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
                            });
                        }
                        // Running: the copy loop notices the flag and reports
                        // through `xfer_done` on the normal terminal path.
                        CancelOutcome::WasRunning => {}
                        CancelOutcome::Unknown => {}
                    },
                    Command::Disconnect => {
                        // A disconnect also clears the single-flight slot.
                        in_flight = false;
                        // Cancel everything: flag the running transfers (their
                        // tasks wind down and report Cancelled via `xfer_done`)
                        // and drain the queued ones (no task ran, so emit their
                        // terminal Cancelled here) — then drop the session (M5 D11).
                        for id in queue.cancel_all() {
                            last_bytes.remove(&id);
                            let _ = events.unbounded_send(Event::TransferDone {
                                id,
                                status: TransferStatus::Cancelled,
                                message: None,
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
            Some(done) = done_rx.recv() => {
                // Any terminal outcome clears the single-flight slot.
                in_flight = false;
                match done {
                    TaskOutcome::Connected { profile_id, client: session } => {
                        client = Some(Arc::from(session));
                        info!(%profile_id, "connected");
                        let _ = events.unbounded_send(Event::Connected { profile_id });
                    }
                    TaskOutcome::ConnectFailed { message } => {
                        let _ = events.unbounded_send(Event::Error { message });
                    }
                    TaskOutcome::TestResult { profile_id, ok, message } => {
                        let _ = events.unbounded_send(Event::TestResult { profile_id, ok, message });
                    }
                }
            }
            Some((id, outcome)) = xfer_done_rx.recv() => {
                // A copy task finished: free its slot, drop its speed counter,
                // announce the terminal state, then backfill the freed slot.
                queue.finish(id);
                last_bytes.remove(&id);
                let (status, message) = match outcome {
                    TransferOutcome::Completed => (TransferStatus::Completed, None),
                    TransferOutcome::Cancelled => (TransferStatus::Cancelled, None),
                    TransferOutcome::Failed(msg) => (TransferStatus::Failed, Some(msg)),
                };
                let _ = events.unbounded_send(Event::TransferDone { id, status, message });
                try_start(&mut queue, &client, &events, &xfer_done_tx);
            }
            _ = progress_tick.tick() => {
                // Sample every running transfer's byte counter and emit a
                // throttled progress event with an instantaneous speed (D12).
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

/// The terminal outcome of a spawned copy task, reported to the dispatcher.
enum TransferOutcome {
    /// The copy finished and the remote writes were acknowledged.
    Completed,
    /// The copy was cancelled mid-flight (a partial file was cleaned up).
    Cancelled,
    /// The copy failed; the credential-free message is for the UI.
    Failed(String),
}

/// Promote and spawn as many queued transfers as the cap allows.
///
/// A missing session is a guard, not an error: queued transfers only exist while
/// connected (the senders check), and `Disconnect` drains the queue — so this is
/// just belt-and-braces against promoting a transfer with no session to run it.
fn try_start(
    queue: &mut TransferQueue,
    client: &Option<Arc<SftpClient>>,
    events: &FuturesSender<Event>,
    xfer_done: &TokioSender<(TransferId, TransferOutcome)>,
) {
    let Some(client) = client else { return };
    while let Some(started) = queue.poll_start() {
        spawn_transfer(client.clone(), started, events.clone(), xfer_done.clone());
    }
}

/// Spawn the copy task for a just-started transfer: stat the size, announce the
/// start, run the protocol copy, clean up any partial file on cancel/fail, and
/// report the terminal outcome back to the dispatcher (plan M5 D6/D8).
fn spawn_transfer(
    client: Arc<SftpClient>,
    started: Started,
    events: FuturesSender<Event>,
    xfer_done: TokioSender<(TransferId, TransferOutcome)>,
) {
    let Started { id, spec, progress } = started;
    tokio::spawn(async move {
        // Stat the total up front so the dock can show a real %/total.
        let total = match spec.direction {
            TransferDirection::Download => client.remote_size(&spec.remote).await,
            TransferDirection::Upload => {
                tokio::fs::metadata(&spec.local).await.ok().map(|m| m.len())
            }
        };
        let _ = events.unbounded_send(Event::TransferStarted { id, total });

        let result = match spec.direction {
            TransferDirection::Download => {
                client.download(&spec.remote, &spec.local, &progress).await
            }
            TransferDirection::Upload => client.upload(&spec.local, &spec.remote, &progress).await,
        };

        let outcome = match result {
            Ok(()) => TransferOutcome::Completed,
            Err(NyxError::Cancelled) => {
                cleanup_partial(&client, &spec).await;
                TransferOutcome::Cancelled
            }
            Err(err) => {
                cleanup_partial(&client, &spec).await;
                TransferOutcome::Failed(err.to_string())
            }
        };
        let _ = xfer_done.send((id, outcome));
    });
}

/// Best-effort removal of the half-written file left by a cancelled/failed
/// transfer (plan M5 D8): the local destination for a download, the remote
/// destination for an upload. Errors are logged at `debug` and never surfaced —
/// the terminal `TransferDone` already tells the user the real story.
async fn cleanup_partial(client: &SftpClient, spec: &TransferSpec) {
    match spec.direction {
        TransferDirection::Download => {
            if let Err(err) = tokio::fs::remove_file(&spec.local).await {
                debug!(error = %err, "could not remove partial download");
            }
        }
        TransferDirection::Upload => {
            if let Err(err) = client.remove(&spec.remote).await {
                debug!(error = %err, "could not remove partial upload");
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

/// The last non-empty path segment (the file/folder name) for toast copy.
/// Paths are not secrets, so this is safe to surface.
fn base_name(path: &str) -> &str {
    path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path)
}

/// Spawn a slow file op as a detached task: run `fut` against the cloned session,
/// then emit [`Event::FileOpDone`] on success or [`Event::Error`] on failure.
///
/// The op never touches the dispatcher's session slot or single-flight guard, so
/// several can run concurrently (`russh-sftp` multiplexes over the one channel).
/// M5 replaces this bare spawn with the `nyx-transfer` scheduler (plan D1).
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

/// Run a single connect-like attempt and report the outcome to the dispatcher.
///
/// For [`TaskKind::Connect`] a success hands the live session back; for
/// [`TaskKind::Test`] the client is dropped and only a credential-free
/// [`TaskOutcome::TestResult`] is reported (no `Connecting` event, so the test
/// never disturbs the UI's connection state).
async fn run_task(
    kind: TaskKind,
    profile: Profile,
    password: Secret,
    events: FuturesSender<Event>,
    register: TokioSender<oneshot::Sender<bool>>,
    done: TokioSender<TaskOutcome>,
) {
    let profile_id = profile.id.clone();
    info!(host = %profile.host, port = profile.port, test = kind == TaskKind::Test, "connecting");
    if kind == TaskKind::Connect {
        let _ = events.unbounded_send(Event::Connecting {
            profile_id: profile_id.clone(),
        });
    }

    let prompt = std::sync::Arc::new(host_key::PromptBridge {
        events: events.clone(),
        register,
    });
    let mut client = SftpClient::new(
        profile.host.clone(),
        profile.port,
        profile.username.clone(),
        password.expose().to_string(),
        KnownHosts::at(known_hosts()),
        prompt,
    );

    let outcome = match (kind, client.connect().await) {
        (TaskKind::Connect, Ok(())) => TaskOutcome::Connected {
            profile_id,
            client: Box::new(client),
        },
        (TaskKind::Connect, Err(err)) => TaskOutcome::ConnectFailed {
            message: err.to_string(),
        },
        (TaskKind::Test, Ok(())) => {
            // The transient client is dropped here (its `Drop` closes the
            // connection), never touching the stored session.
            let _ = client.disconnect().await;
            TaskOutcome::TestResult {
                profile_id,
                ok: true,
                message: "Connection OK".into(),
            }
        }
        (TaskKind::Test, Err(err)) => TaskOutcome::TestResult {
            profile_id,
            ok: false,
            message: err.to_string(),
        },
    };
    let _ = done.send(outcome);
}

/// The service-side host-key prompt: surface a [`Event::HostKeyPrompt`] to the UI
/// and await the user's [`Command::HostKeyDecision`].
mod host_key {
    use super::*;
    use nyx_protocol::HostKeyPrompt;

    /// Bridges the protocol layer's host-key callback to the UI event/command flow.
    pub struct PromptBridge {
        pub events: FuturesSender<Event>,
        pub register: TokioSender<oneshot::Sender<bool>>,
    }

    #[async_trait::async_trait]
    impl HostKeyPrompt for PromptBridge {
        async fn confirm_unknown(&self, host: &str, fingerprint: &str) -> bool {
            let (responder, answer) = oneshot::channel();
            // Register the responder with the dispatcher *before* prompting, so a
            // decision can never arrive with no slot to resolve.
            if self.register.send(responder).is_err() {
                return false;
            }
            let _ = self.events.unbounded_send(Event::HostKeyPrompt {
                host: host.to_string(),
                fingerprint: fingerprint.to_string(),
            });
            // A dropped sender (e.g. shutdown) resolves to "do not trust".
            answer.await.unwrap_or(false)
        }
    }
}
