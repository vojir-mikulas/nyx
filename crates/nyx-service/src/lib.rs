// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The Nyx backend service.
//!
//! GPUI runs its own executor on the main thread; `russh` (the future SFTP
//! transport) is Tokio-based. So the backend lives on a **dedicated thread**
//! that owns a Tokio runtime, all connections and the transfer queue. The UI
//! talks to it over two channels:
//!
//! - [`Command`] — UI → service (sent synchronously from the GPUI thread).
//! - [`Event`] — service → UI (the UI drains these into its views).
//!
//! This module is the skeleton: it establishes the thread + runtime + channel
//! pattern. No real protocol work happens yet — the command loop just
//! acknowledges and handles shutdown.

use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::thread::{self, JoinHandle};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// A request from the UI to the backend.
///
/// Variants for connect/list/transfer land as the protocol work does; the
/// command/event channel pattern is what we establish now.
#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    /// Ask the service to shut down its runtime and exit the thread.
    Shutdown,
}

/// A message from the backend to the UI.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// The backend thread and Tokio runtime are up.
    Ready,
    /// The backend has stopped (after a [`Command::Shutdown`] or channel drop).
    Stopped,
}

/// Handle to the running backend thread.
///
/// Send [`Command`]s with [`ServiceHandle::send`]. Dropping the handle requests
/// shutdown and joins the thread.
pub struct ServiceHandle {
    commands: UnboundedSender<Command>,
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
/// (service → UI) the UI drains. The `Event` side is a `std` channel so the GPUI
/// thread can poll it without entering an async context.
pub fn spawn() -> (ServiceHandle, StdReceiver<Event>) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
    let (evt_tx, evt_rx) = std::sync::mpsc::channel::<Event>();

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

/// The backend thread entry point: build the runtime and drive the command loop.
fn run(mut commands: UnboundedReceiver<Command>, events: StdSender<Event>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .thread_name("nyx-service-worker")
        .build()
        .expect("failed to build Tokio runtime");

    let _ = events.send(Event::Ready);

    runtime.block_on(async move {
        // Today `Shutdown` is the only command, so the loop trivially exits on
        // the first message; once connect/list/transfer commands land they'll
        // be handled here and continue the loop. Allow the lint until then.
        #[allow(clippy::never_loop)]
        while let Some(command) = commands.recv().await {
            match command {
                Command::Shutdown => break,
            }
        }
    });

    let _ = events.send(Event::Stopped);
}
