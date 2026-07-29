//! Supervisor lifecycle controls: teardown, restart, and the poll
//! pump that turns background events into observable state.
//!
//! Split out of `runtime.rs` in the 500-LOC modularity refactor. Adds
//! a second `impl RuntimeSupervisor` block that merges with the one
//! in [`super::supervisor`] at compile time. Owns:
//! - [`RuntimeSupervisor::stop`] / [`RuntimeSupervisor::restart`]
//! - [`RuntimeSupervisor::poll`] (also runs the iteration-2
//!   bridge-terminal teardown before the regular try_wait pass),
//! - [`RuntimeSupervisor::suspend_session_sink_on_exit`] /
//!   `suspend_session_sink_on_start_failure`, and
//! - the `#[cfg(all(test, feature = "audio-in-rust"))]`
//!   `trigger_bridge_terminal_for_tests` hook.

#[cfg(feature = "audio-in-rust")]
use std::sync::atomic::Ordering;
use std::thread;

use anyhow::Result;

use super::process::kill_child;
use super::rust_session_sink;
use super::supervisor::{RuntimeEvent, RuntimeState, RuntimeSupervisor};
use super::worker_command::WorkerCommand;

impl RuntimeSupervisor {
    pub fn stop(&mut self) -> Result<()> {
        // Stop the Rust audio bridge BEFORE killing the worker. The
        // bridge closes its end of the stdin pipe; the worker's
        // RustStdinAudioSource sees EOF and finishes the current
        // utterance (no half-buffered audio). If we killed the worker
        // first the bridge would race the kill with a write and emit
        // a spurious `WorkerClosed` event.
        #[cfg(feature = "audio-in-rust")]
        {
            // Iteration-3 race-guard: tell any in-flight ready-watch
            // thread to abort BEFORE we look at the slot. If it's
            // mid-`pending.start()`, it'll see this and drop the
            // freshly-built handle on completion instead of
            // installing it into a stopped supervisor.
            self.bridge_cancel.store(true, Ordering::SeqCst);
            if let Some(mut bridge) = self.audio_bridge.lock().ok().and_then(|mut s| s.take()) {
                bridge.stop();
            }
        }

        // Fix 4 (#373): suspend Rust hotkey tracking while the worker is
        // down so PTT presses during a stopped period don't leave the
        // coordinator in Recording state at the next start(). The manager
        // unregisters its binding so no tracker outputs flow; Cancel resets
        // Recording → Idle. A coordinator stuck in Processing (transcription
        // was in-flight at stop) remains there until the next
        // ProcessingFinished — that is acceptable because Python handles
        // actual recording so correctness is unaffected.
        if let Some(handle) = self.hotkey_handle.as_ref() {
            handle.suspend();
        }

        let Some(mut child) = self.child.take() else {
            // Nothing is running, so nothing can still be listening:
            // release immediately.
            self.python_ptt_lock = None;
            self.state = RuntimeState::Stopped;
            return Ok(());
        };

        // Hand the ownership token to the reaper rather than dropping it
        // here (Codex P1 #688). The child is killed asynchronously below,
        // and until `kill_child` + `wait` complete the worker may STILL
        // have pynput / evdev registered. Releasing synchronously would
        // open a window in which a `dictate-run` -- or the replacement
        // worker on a `restart()` -- acquires the freed lock while the old
        // listener is alive, which is exactly the concurrent-injection
        // state the guard exists to prevent. Moving the lock into the
        // thread means it is dropped only after the child is reaped, on
        // both the success and the failure path.
        let handoff = self.python_ptt_lock.take();
        // Lets the next `start()` wait for the release instead of racing
        // it; see `RuntimeSupervisor::await_python_ptt_release`.
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        self.python_ptt_release = Some(release_rx);

        self.state = RuntimeState::Stopped;
        let tx = self.tx.clone();
        let notifier = self.repaint_notifier.clone();
        thread::spawn(move || {
            let result = kill_child(&mut child).and_then(|_| child.wait().map_err(Into::into));
            // Explicit, and before the events below, so the ordering
            // ("child is gone, THEN the chord is free") is visible rather
            // than left to the drop order at the end of the closure.
            drop(handoff);
            let _ = release_tx.send(());
            match result {
                Ok(status) => {
                    let _ = tx.send(RuntimeEvent::Exited {
                        code: status.code(),
                    });
                }
                Err(err) => {
                    let _ = tx.send(RuntimeEvent::Error(format!("stop failed: {err}")));
                }
            }
            if let Some(notifier) = notifier.as_ref() {
                notifier();
            }
        });
        Ok(())
    }

    pub fn restart(&mut self, command: WorkerCommand) -> Result<()> {
        self.stop()?;
        self.start(command)
    }

    /// Test-only hook (crate-visible): simulate the bridge-error
    /// watcher observing a terminal `BridgeError::Io` /
    /// `BridgeError::Pipeline`. The next `poll()` will then run the
    /// iteration-2 review finding #3 teardown path (kill the child,
    /// drop the bridge, synthesize `Exited`). Crate-visible so
    /// `runtime/bridge_terminal_tests.rs` can exercise the path
    /// without spinning up a real cpal failure.
    #[cfg(all(test, feature = "audio-in-rust"))]
    #[doc(hidden)]
    pub(crate) fn trigger_bridge_terminal_for_tests(&self) {
        self.bridge_terminal.store(true, Ordering::SeqCst);
    }

    /// When the rust-session sink is active, suspend the hotkey
    /// handle on an unexpected child exit so PTT presses do not keep
    /// driving the in-process [`crate::dictate::DictateSession`] while
    /// the UI considers the runtime stopped. Codex P2 #416
    /// runtime.rs:1484.
    ///
    /// No-op for the logger-sink path: the logger sink is harmless
    /// (just stderr lines), and Python -- which owned the recording
    /// lifecycle in that path -- already exited together with the
    /// child. Leaving the Rust manager registered there preserves the
    /// existing PR 1-3 behaviour exactly.
    ///
    /// The next `start()` call's restart-path branch then re-registers
    /// the binding via `handle.resume(key_names)` so PTT comes back
    /// online with the (possibly updated) chord.
    pub(super) fn suspend_session_sink_on_exit(&mut self) {
        // Ungated, unlike the session-sink suspend below: the Python
        // worker holds no session sink, and a lock we took on its behalf
        // must come back the moment it stops listening — whether it
        // exited cleanly, crashed, or was killed (Codex P1 #688).
        // Otherwise a crashed worker would leave the chord reserved by a
        // process that has nothing registered, and every later
        // `dictate-run` would be refused for no reason.
        self.python_ptt_lock = None;

        if !rust_session_sink::dictate_backend_rust_session_requested() {
            return;
        }
        if let Some(handle) = self.hotkey_handle.as_ref() {
            handle.suspend();
        }
    }

    /// Mirror of [`Self::suspend_session_sink_on_exit`] for the
    /// `start()` error-return paths. Codex P2 #416 (round 2)
    /// runtime.rs:504: the Rust hotkey handle is installed (and the
    /// session sink registered with the coordinator) BEFORE the
    /// fallible `process.spawn()` + audio-bridge prep steps. If those
    /// fail and `start()` returns Err, the UI flips to Stopped but
    /// the hotkey handle is still live -- PTT presses can still drive
    /// `DictateSession` against a worker that never started.
    ///
    /// Suspend the handle on those paths so the coordinator returns
    /// to Idle and PTT goes silent until the next successful start.
    /// Same gate as the on-exit path: only the session-sink build
    /// needs cleanup (the logger sink is inert).
    pub(super) fn suspend_session_sink_on_start_failure(&mut self) {
        self.suspend_session_sink_on_exit();
    }

    pub fn poll(&mut self) -> Vec<RuntimeEvent> {
        // Iteration-2 review finding #3: act on a terminal bridge error
        // BEFORE the regular try_wait. The bridge watcher has already
        // emitted a `RuntimeEvent::Error` describing the failure; here
        // we follow up with the teardown the watcher couldn't perform
        // from a background thread (it has no `&mut self`). Kill the
        // child, drop the bridge handle, and synthesize an `Exited`
        // so the UI flips back to Stopped on its next poll.
        #[cfg(feature = "audio-in-rust")]
        if self.bridge_terminal.swap(false, Ordering::SeqCst) {
            self.bridge_cancel.store(true, Ordering::SeqCst);
            if let Some(mut bridge) = self.audio_bridge.lock().ok().and_then(|mut s| s.take()) {
                bridge.stop();
            }
            if let Some(mut child) = self.child.take() {
                let _ = kill_child(&mut child);
                let exit_code = child.wait().ok().and_then(|status| status.code());
                self.state = RuntimeState::Stopped;
                // Codex P2 #416 (round 2) runtime.rs:875 -- the
                // try_wait arms call this on every unexpected exit;
                // the bridge-terminal branch kills+takes the child
                // and returns BEFORE those arms run, so without this
                // call a terminal BridgeError would leave the
                // session sink driving DictateSession while the UI
                // shows Stopped.
                self.suspend_session_sink_on_exit();
                let _ = self.tx.send(RuntimeEvent::Exited { code: exit_code });
                if let Some(notifier) = self.repaint_notifier.as_ref() {
                    notifier();
                }
            } else {
                // No child to kill (already exited): just be sure
                // state reflects stopped.
                self.state = RuntimeState::Stopped;
                // Same Codex P2 (round 2) -- even if the child was
                // already gone, the hotkey handle could still be
                // live.
                self.suspend_session_sink_on_exit();
            }
            return self.rx.try_iter().collect();
        }

        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.state = RuntimeState::Stopped;
                    self.child = None;
                    // The worker exited (crash, --doctor finished,
                    // user-killed, …). Tear the audio bridge down so
                    // it doesn't keep cpal open against a missing
                    // reader. Same teardown order as `stop()`.
                    #[cfg(feature = "audio-in-rust")]
                    {
                        self.bridge_cancel.store(true, Ordering::SeqCst);
                        if let Some(mut bridge) =
                            self.audio_bridge.lock().ok().and_then(|mut s| s.take())
                        {
                            bridge.stop();
                        }
                    }
                    self.suspend_session_sink_on_exit();
                    let _ = self.tx.send(RuntimeEvent::Exited {
                        code: status.code(),
                    });
                }
                Ok(None) => {}
                Err(err) => {
                    self.state = RuntimeState::Stopped;
                    self.child = None;
                    #[cfg(feature = "audio-in-rust")]
                    {
                        self.bridge_cancel.store(true, Ordering::SeqCst);
                        if let Some(mut bridge) =
                            self.audio_bridge.lock().ok().and_then(|mut s| s.take())
                        {
                            bridge.stop();
                        }
                    }
                    self.suspend_session_sink_on_exit();
                    let _ = self.tx.send(RuntimeEvent::Error(err.to_string()));
                }
            }
        }

        self.rx.try_iter().collect()
    }
}
