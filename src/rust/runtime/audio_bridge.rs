//! Feature-gated bridge between the Python worker's stdin and the
//! Rust-side cpal capture pipeline.
//!
//! Split out of `runtime.rs` in the 500-LOC modularity refactor. Owns
//! the three [`RuntimeSupervisor`] methods that only compile with
//! `--features audio-in-rust`: the ready-watch thread that defers
//! opening cpal until the worker is up
//! ([`RuntimeSupervisor::spawn_ready_watch`]), the crate-visible
//! error-watch shim retained for tests
//! ([`RuntimeSupervisor::spawn_audio_bridge_error_watch`]), and the
//! pure error-translation loop
//! ([`RuntimeSupervisor::run_bridge_error_loop`]).

#![cfg(feature = "audio-in-rust")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;

use super::supervisor::{RepaintNotifier, RuntimeEvent, RuntimeSupervisor};

impl RuntimeSupervisor {
    /// Iteration-3 review finding #1: park the prepared [`crate::audio::PendingBridge`]
    /// on a background thread that waits for the Python worker's
    /// `state=ready` event before opening cpal and starting the writer.
    /// This avoids the race where the supervisor would otherwise emit
    /// VAD-detected speech frames into the child's stdin DURING the
    /// child's model load (the Python reader doesn't exist until
    /// `Dictate.__init__`, which runs after the model is ready).
    ///
    /// On `ready_rx` ping: open cpal, install the live `BridgeHandle`
    /// into `self.audio_bridge`, and start the error-watcher. If the
    /// receiver hangs up before a ping (worker died / supervisor
    /// stopped during model load), the `PendingBridge` is dropped here
    /// — no cpal stream is ever opened, preserving the user's
    /// mic-permission state.
    pub(super) fn spawn_ready_watch(
        &self,
        pending: crate::audio::PendingBridge,
        device: String,
        ready_rx: Receiver<()>,
    ) {
        let tx = self.tx.clone();
        let notifier = self.repaint_notifier.clone();
        let bridge_slot = self.audio_bridge.clone();
        let terminal = self.bridge_terminal.clone();
        let cancel = self.bridge_cancel.clone();
        thread::spawn(move || {
            if ready_rx.recv().is_err() {
                // Worker died (or `stop()` torpedoed the streamer)
                // before emitting ready. Drop the pending bridge —
                // cpal never opened, so the user's mic-permission
                // state is preserved for the next run.
                let _ = tx.send(RuntimeEvent::Stderr(
                    "[runtime] audio-in-rust: worker exited before ready; \
                     bridge cancelled (no cpal stream opened)"
                        .to_owned(),
                ));
                drop(pending);
                return;
            }
            // Race-guard: if `stop()` ran while we were parked on the
            // ready signal, don't open cpal — the supervisor is on its
            // way down and doesn't want the bridge any more.
            if cancel.load(Ordering::SeqCst) {
                drop(pending);
                return;
            }
            match pending.start(&device) {
                Ok((bridge, errors)) => {
                    // Recheck the cancel flag AFTER cpal opened. If
                    // `stop()` raced us between the load above and
                    // here, the supervisor already locked the bridge
                    // slot and moved on with nothing to teardown.
                    // Drop the freshly-built handle ourselves to close
                    // cpal + the writer instead of installing it into
                    // a stopped supervisor.
                    if cancel.load(Ordering::SeqCst) {
                        drop(bridge);
                        return;
                    }
                    if let Ok(mut slot) = bridge_slot.lock() {
                        *slot = Some(bridge);
                    }
                    let _ = tx.send(RuntimeEvent::Stderr(
                        "[runtime] audio-in-rust: Rust capture pipeline active for this run"
                            .to_owned(),
                    ));
                    Self::run_bridge_error_loop(errors, tx, notifier, terminal);
                }
                Err(err) => {
                    // cpal open failed (mic in use / unplugged) AFTER
                    // the worker came up ready. Surface as an Error
                    // event and flag the supervisor for teardown so the
                    // UI stops claiming we're recording. Same teardown
                    // path as a runtime bridge error.
                    let _ = tx.send(RuntimeEvent::Error(format!(
                        "audio-in-rust: failed to open capture stream: {err}; \
                         unset VOICEPI_AUDIO_BACKEND to fall back"
                    )));
                    terminal.store(true, Ordering::SeqCst);
                    if let Some(notifier) = notifier.as_ref() {
                        notifier();
                    }
                }
            }
        });
    }

    /// Watch the audio bridge's error channel in a background thread
    /// and translate any [`crate::audio::BridgeError`] into a
    /// [`RuntimeEvent::Error`] (UI surfaces it) or a stderr trace
    /// (expected `WorkerClosed` on PTT release). Stops as soon as the
    /// bridge's channel closes — the bridge sends AT MOST ONE error
    /// per run, so this watcher is a tight one-shot loop. Fire-and-
    /// forget: dropping the bridge closes the channel and the watcher
    /// exits naturally.
    #[allow(dead_code)] // Retained for tests; production path uses run_bridge_error_loop directly.
    pub(super) fn spawn_audio_bridge_error_watch(
        &self,
        errors: std::sync::mpsc::Receiver<crate::audio::BridgeError>,
    ) {
        let tx = self.tx.clone();
        let notifier = self.repaint_notifier.clone();
        let terminal = self.bridge_terminal.clone();
        // Flag-up the active backend on the supervisor channel so the
        // user can tell from the runtime log which path actually ran.
        let _ = tx.send(RuntimeEvent::Stderr(
            "[runtime] audio-in-rust: Rust capture pipeline active for this run".to_owned(),
        ));
        thread::spawn(move || {
            Self::run_bridge_error_loop(errors, tx, notifier, terminal);
        });
    }

    /// Pure error-translation loop. Extracted so the iteration-3
    /// ready-watch thread (which already owns the bridge-creation
    /// site) can drive it inline without spawning a second thread —
    /// and so unit tests can drive it without a real bridge.
    pub(super) fn run_bridge_error_loop(
        errors: std::sync::mpsc::Receiver<crate::audio::BridgeError>,
        tx: Sender<RuntimeEvent>,
        notifier: Option<RepaintNotifier>,
        terminal: Arc<AtomicBool>,
    ) {
        use crate::audio::BridgeError;
        while let Ok(err) = errors.recv() {
            // Iteration-2 review finding #3: `Io` and `Pipeline` are
            // TERMINAL — the writer has already dropped the child's
            // stdin handle and exited, so even if the worker is
            // technically still alive it can no longer receive audio.
            // Raise the teardown flag so the next `poll()` kills the
            // child and surfaces an `Exited`, flipping the UI back to
            // Stopped instead of leaving the user staring at a
            // running-but-deaf worker.
            let is_terminal = matches!(err, BridgeError::Io(_) | BridgeError::Pipeline(_));
            let event = match err {
                // WorkerClosed = the Python child closed stdin (normal
                // teardown). Surface as a stderr trace, not an Error
                // event, so the UI doesn't pop a false-positive failure
                // banner on every PTT release.
                BridgeError::WorkerClosed => RuntimeEvent::Stderr(
                    "[runtime] audio-in-rust: worker closed audio stdin (normal teardown)"
                        .to_owned(),
                ),
                BridgeError::Io(msg) => RuntimeEvent::Error(format!(
                    "audio-in-rust: failed writing to worker stdin: {msg}"
                )),
                BridgeError::Pipeline(msg) => {
                    RuntimeEvent::Error(format!("audio-in-rust: capture pipeline error: {msg}"))
                }
            };
            let _ = tx.send(event);
            if is_terminal {
                terminal.store(true, Ordering::SeqCst);
            }
            if let Some(notifier) = notifier.as_ref() {
                notifier();
            }
        }
    }
}
