//! Driver-agnostic plumbing shared by every OS-listener backend
//! ([`super::rdev_driver`] on X11 / Windows / macOS, [`super::evdev_driver`]
//! on Linux/Wayland).
//!
//! A "driver" has two halves:
//!
//! * a **listener** half that owns the platform global-key hook, blocks on it
//!   for the process lifetime, and pushes each translated [`crate::hotkey::manager::RawKeyEvent`]
//!   through a shared `Mutex<KeyTracker>` into an `on_output` sink — this half
//!   is platform-specific and lives in each driver module; and
//! * a **manager** half — a thread that owns the same `Mutex<KeyTracker>` and
//!   services `register` / `unregister` / `shutdown` commands over an mpsc so
//!   the rest of the runtime (which is not on the listener thread and cannot
//!   touch the non-`Send` native handle) can swap the active PTT binding.
//!
//! The manager half is byte-identical across backends, so it lives here along
//! with the public [`ManagerHandle`] / [`ManagerThread`] / [`SpawnError`]
//! contract every driver's `spawn` returns. Both drivers construct their
//! sender via [`manager_channel`] and start the manager thread via
//! [`spawn_manager_thread`]; the split lets the platform-specific listener
//! half wire itself to the same `Arc<Mutex<KeyTracker>>` before the manager
//! thread begins servicing commands.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::tracker::{KeyTracker, RawKeyEvent};

// -----------------------------------------------------------------------
// RawTap — driver-agnostic sink for raw OS key events. Both `rdev_driver`
// and `evdev_driver` invoke `tap()` BEFORE the tracker processes each
// event so the diagnostic `whisper-dictate hotkey capture` CLI can wire
// up a listener that surfaces every keydown/keyup regardless of which
// backend the manager selector picked.
// -----------------------------------------------------------------------

/// Marker type for `spawn_with_raw_tap` callers that don't want a raw tap.
/// Cheaper than an `Option<Arc<dyn Fn>>` — the trait dispatch inlines to a
/// no-op so the shipping supervisor path pays nothing for the diagnostic
/// hook.
pub struct NoopRawTap;

/// Sink for raw OS key events, called BEFORE the tracker processes them.
/// Implemented by [`NoopRawTap`] (drops silently) and any `Fn(&RawKeyEvent) +
/// Send + Sync + 'static` closure. Kept as a trait so both drivers'
/// `spawn_with_raw_tap` can accept either without runtime overhead — the
/// diagnostic `whisper-dictate hotkey capture` CLI wires up a real tap so
/// the operator can see every keydown/keyup, not just the chord-level
/// output the coordinator produces.
///
/// Lives here (not in a driver module) so the trait is identical across
/// backends — otherwise `manager::spawn_with_raw_tap`'s dispatch to
/// `evdev_driver::spawn_with_raw_tap` wouldn't type-check with the R
/// bound coming from the rdev module.
pub trait RawTap: Send + Sync + 'static {
    fn tap(&self, event: &RawKeyEvent);
}

impl RawTap for NoopRawTap {
    #[inline]
    fn tap(&self, _event: &RawKeyEvent) {}
}

impl<F> RawTap for F
where
    F: Fn(&RawKeyEvent) + Send + Sync + 'static,
{
    #[inline]
    fn tap(&self, event: &RawKeyEvent) {
        (self)(event);
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    #[test]
    fn register_and_unregister_roundtrip_through_manager_thread() {
        // Round-trip a Register / Unregister / Register cycle through the
        // manager thread WITHOUT any OS listener attached — proves the
        // shared plumbing is driver-agnostic and does not depend on rdev
        // or evdev being present. Both drivers' `spawn` builds on this
        // exact channel + spawn helper pair.
        let (handle, cmd_rx) = manager_channel();
        let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));
        let thread =
            spawn_manager_thread(cmd_rx, Arc::clone(&tracker)).expect("manager thread spawns");

        handle
            .register(vec!["ctrl_l".to_owned(), "f9".to_owned()])
            .expect("register");
        handle.unregister().expect("unregister");
        handle
            .register(vec!["shift_r".to_owned()])
            .expect("re-register");

        // Give the manager thread a beat, then shutdown.
        handle.shutdown();
        thread.join();
    }

    #[test]
    fn spawn_error_display_variants_carry_context() {
        // The message payload of `ListenerStartup` is what surfaces to the
        // supervisor / CLI, so it must be preserved verbatim through the
        // Display impl. `ListenerHung` has no payload but should still
        // render.
        let msg = SpawnError::ListenerStartup("no X display".to_owned()).to_string();
        assert!(msg.contains("no X display"), "context lost: {msg}");
        let hung = SpawnError::ListenerHung.to_string();
        assert!(!hung.is_empty(), "hung variant must render");
    }

    #[test]
    fn shutdown_on_disconnected_handle_is_a_noop() {
        // If the manager thread already exited, `shutdown` must not panic
        // (the send silently fails). This is the drop-order guarantee the
        // `HotkeyHandle` shutdown relies on.
        let (handle, cmd_rx) = manager_channel();
        let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));
        let thread = spawn_manager_thread(cmd_rx, tracker).expect("manager thread spawns");
        handle.shutdown();
        thread.join();
        // Second shutdown after the thread is gone — must not panic.
        handle.shutdown();
        std::thread::sleep(StdDuration::from_millis(1));
    }
}
