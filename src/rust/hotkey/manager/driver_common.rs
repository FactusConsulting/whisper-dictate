//! Driver-agnostic plumbing shared by every OS-listener backend
//! ([`super::rdev_driver`] on X11 / Windows / macOS, [`super::evdev_driver`]
//! on Linux/Wayland).
//!
//! A "driver" has two halves:
//!
//! * a **listener** half that owns the platform global-key hook, blocks on it
//!   for the process lifetime, and pushes each translated [`RawKeyEvent`]
//!   through a shared `Mutex<KeyTracker>` into an `on_output` sink — this half
//!   is platform-specific and lives in each driver module; and
//! * a **manager** half — a thread that owns the same `Mutex<KeyTracker>` and
//!   services `register` / `unregister` / `shutdown` commands over an mpsc so
//!   the rest of the runtime (which is not on the listener thread and cannot
//!   touch the non-`Send` native handle) can swap the active PTT binding.
//!
//! The manager half is byte-identical across backends, so it lives here along
//! with the public [`ManagerHandle`] / [`ManagerThread`] / [`SpawnError`]
//! contract every driver's `spawn` returns.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::tracker::KeyTracker;

/// Commands the manager thread accepts on its inbound channel. Each carries
/// a sync response sender so the caller can `recv()` confirmation from a
/// non-listener thread; this is the "mpsc commands with sync response"
/// pattern issue #318 calls out.
pub enum ManagerCommand {
    Register {
        targets: Vec<String>,
        ack: Sender<Result<(), String>>,
    },
    Unregister {
        ack: Sender<Result<(), String>>,
    },
    Shutdown,
}

/// Public handle to the manager thread. Backend-agnostic: both the rdev and
/// the evdev driver hand one of these back from `spawn`.
#[derive(Clone)]
pub struct ManagerHandle {
    tx: Sender<ManagerCommand>,
}

impl ManagerHandle {
    /// Install (or replace) the active PTT binding. Blocks until the manager
    /// thread acknowledges, but the underlying operation is cheap — just
    /// swapping a `Vec<String>` in a mutex.
    pub fn register(&self, targets: Vec<String>) -> Result<(), String> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.tx
            .send(ManagerCommand::Register {
                targets,
                ack: ack_tx,
            })
            .map_err(|e| format!("manager thread disconnected: {e}"))?;
        ack_rx
            .recv()
            .map_err(|e| format!("ack channel closed: {e}"))?
    }

    /// Stop emitting tracker outputs without tearing down the listener
    /// thread (the OS listener stays installed — neither `rdev` nor the
    /// blocking evdev reads give us a clean per-binding teardown — but the
    /// tracker is replaced with an empty one so no events flow through).
    pub fn unregister(&self) -> Result<(), String> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.tx
            .send(ManagerCommand::Unregister { ack: ack_tx })
            .map_err(|e| format!("manager thread disconnected: {e}"))?;
        ack_rx
            .recv()
            .map_err(|e| format!("ack channel closed: {e}"))?
    }

    /// Ask the manager thread to exit. The OS-level listener thread(s) cannot
    /// be interrupted (rdev's `listen()` / evdev's blocking `fetch_events()`
    /// both park forever), so they leak on shutdown — acceptable because the
    /// supervisor only ever installs the hotkey subsystem once and tears down
    /// on process exit.
    pub fn shutdown(&self) {
        let _ = self.tx.send(ManagerCommand::Shutdown);
    }
}

/// Construct a [`ManagerHandle`] plus its inbound receiver. The driver passes
/// the receiver to [`spawn_manager_thread`]; keeping the split here means the
/// listener half can be wired to the same `Arc<Mutex<KeyTracker>>` before the
/// manager thread starts.
pub fn manager_channel() -> (ManagerHandle, Receiver<ManagerCommand>) {
    let (tx, rx) = mpsc::channel();
    (ManagerHandle { tx }, rx)
}

/// Owned join handle for the manager thread (NOT the inner OS listener
/// thread, which cannot be joined). Kept by the caller for cleanup.
pub struct ManagerThread {
    join: Option<JoinHandle<()>>,
}

impl ManagerThread {
    pub fn join(mut self) {
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

/// Errors a driver's `spawn` surfaces on startup. Translated to
/// `InstallError` by the hotkey module so the supervisor can pick a fallback
/// strategy.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("hotkey listener startup failed: {0}")]
    ListenerStartup(String),
    #[error("hotkey listener thread never reported it was started")]
    ListenerHung,
}

/// Spawn the manager thread that owns `tracker` and services register /
/// unregister / shutdown commands off `cmd_rx`. Returns the joinable handle
/// wrapper; the listener half must already share `tracker` via `Arc::clone`.
pub fn spawn_manager_thread(
    cmd_rx: Receiver<ManagerCommand>,
    tracker: Arc<Mutex<KeyTracker>>,
) -> Result<ManagerThread, SpawnError> {
    let join = thread::Builder::new()
        .name("vp-hotkey-manager".to_owned())
        .spawn(move || manager_loop(cmd_rx, tracker))
        .map_err(|e| SpawnError::ListenerStartup(format!("manager thread spawn failed: {e}")))?;
    Ok(ManagerThread { join: Some(join) })
}

fn manager_loop(rx: Receiver<ManagerCommand>, tracker: Arc<Mutex<KeyTracker>>) {
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(ManagerCommand::Register { targets, ack }) => {
                *tracker.lock().expect("tracker poisoned") = KeyTracker::new(targets);
                let _ = ack.send(Ok(()));
            }
            Ok(ManagerCommand::Unregister { ack }) => {
                *tracker.lock().expect("tracker poisoned") = KeyTracker::new(Vec::new());
                let _ = ack.send(Ok(()));
            }
            Ok(ManagerCommand::Shutdown) => return,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
