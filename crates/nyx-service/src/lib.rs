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

mod host_key_path;

use std::fmt;
use std::thread::{self, JoinHandle};

use futures::channel::mpsc::{
    unbounded as futures_unbounded, UnboundedReceiver as FuturesReceiver,
    UnboundedSender as FuturesSender,
};
use nyx_core::RemoteEntry;
use nyx_profile::Profile;
use nyx_protocol::{KnownHosts, RemoteClient, SftpClient};
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver as TokioReceiver, UnboundedSender as TokioSender,
};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use crate::host_key_path::known_hosts;

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
    /// An operation failed. The message is human-readable and credential-free.
    Error {
        /// The error detail (a `NyxError` display; never contains a secret).
        message: String,
    },
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

/// The result of a connect task, handed back to the dispatcher.
enum ConnectDone {
    /// Connected — the dispatcher takes ownership of the live session.
    Connected {
        profile_id: String,
        client: Box<SftpClient>,
    },
    /// Connect failed with a credential-free message.
    Failed { message: String },
}

/// The single command dispatcher. Owns the active session and the host-key
/// decision slot; connect runs as a spawned task so the loop stays responsive to
/// [`Command::HostKeyDecision`] while a handshake awaits the user.
async fn dispatch(mut commands: TokioReceiver<Command>, events: FuturesSender<Event>) {
    let mut client: Option<Box<SftpClient>> = None;
    // The responder for an in-flight host-key prompt (at most one connect at a
    // time in M2). The connect task registers it here before showing the prompt.
    let mut pending_host_key: Option<oneshot::Sender<bool>> = None;

    // Internal channels: connect task → dispatcher.
    let (register_tx, mut register_rx) = unbounded_channel::<oneshot::Sender<bool>>();
    let (done_tx, mut done_rx) = unbounded_channel::<ConnectDone>();

    loop {
        tokio::select! {
            maybe_cmd = commands.recv() => {
                let Some(command) = maybe_cmd else { break };
                match command {
                    Command::Shutdown => break,
                    Command::Connect { profile, password } => {
                        // Replace any existing session.
                        client = None;
                        tokio::spawn(run_connect(
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
                    Command::Disconnect => {
                        if let Some(mut session) = client.take() {
                            let _ = session.disconnect().await;
                            info!("disconnected");
                        }
                    }
                }
            }
            Some(responder) = register_rx.recv() => {
                pending_host_key = Some(responder);
            }
            Some(done) = done_rx.recv() => {
                match done {
                    ConnectDone::Connected { profile_id, client: session } => {
                        client = Some(session);
                        info!(%profile_id, "connected");
                        let _ = events.unbounded_send(Event::Connected { profile_id });
                    }
                    ConnectDone::Failed { message } => {
                        let _ = events.unbounded_send(Event::Error { message });
                    }
                }
            }
        }
    }
}

/// Run a single connection attempt and report the outcome to the dispatcher.
async fn run_connect(
    profile: Profile,
    password: Secret,
    events: FuturesSender<Event>,
    register: TokioSender<oneshot::Sender<bool>>,
    done: TokioSender<ConnectDone>,
) {
    let profile_id = profile.id.clone();
    info!(host = %profile.host, port = profile.port, "connecting");
    let _ = events.unbounded_send(Event::Connecting {
        profile_id: profile_id.clone(),
    });

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

    let outcome = match client.connect().await {
        Ok(()) => ConnectDone::Connected {
            profile_id,
            client: Box::new(client),
        },
        Err(err) => ConnectDone::Failed {
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
