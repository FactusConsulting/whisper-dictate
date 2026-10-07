//! The owning handle for the Rust hotkey subsystem.
//!
//! [`HotkeyHandle`] keeps the manager and coordinator threads alive,
//! owns the push-to-talk lock for the installed chord, and carries the
//! shutdown order. The handle's lifetime contract sits in one file next
//! to its structural scanners.

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection", test))]
use super::coordinator::spawn as spawn_coordinator;
use super::coordinator::CoordinatorHandle;
#[cfg(feature = "rust-hotkeys")]
use super::coordinator::CoordinatorThread;
#[cfg(all(test, feature = "rust-injection"))]
use super::coordinator::Options;
#[cfg(feature = "rust-hotkeys")]
use super::coordinator::{self, CoordinatorEvent};
#[cfg(feature = "rust-hotkeys")]
use super::inject_guard::InjectionGuard;
#[cfg(feature = "rust-hotkeys")]
use super::manager::{self, ManagerHandle, ManagerThread};
#[cfg(feature = "rust-hotkeys")]
use super::ptt_lock;
#[cfg(all(test, feature = "rust-injection"))]
use std::time::Instant;

#[cfg(feature = "rust-hotkeys")]
use std::sync::Arc;

/// Owning handle for the Rust hotkey subsystem. Drop or
/// [`HotkeyHandle::shutdown`] to tear it all down. The OS listener thread
/// itself cannot be interrupted on every backend (rdev limitation), so a
/// tear-down can leave that OS hook thread until process exit. The manager and
/// coordinator inputs still close synchronously before restart.
#[cfg(feature = "rust-hotkeys")]
pub struct HotkeyHandle {
    pub(super) coordinator: CoordinatorHandle,
    pub(super) coordinator_thread: Option<CoordinatorThread>,
    pub(super) manager: ManagerHandle,
    pub(super) manager_thread: Option<ManagerThread>,
    /// Shared self-injection guard. The driver callback reads this to
    /// drop OS events that the app's own text injector is currently
    /// producing (Windows self-injection PTT wedge; see
    /// [`inject_guard`] for the full rationale). Exposed via
    /// [`HotkeyHandle::injection_guard`] so the runtime can hand the
    /// same handle to the injector — arms on the injector side, checks
    /// on the driver side. On the evdev backend it is the belt-and-braces
    /// second layer behind device-enumeration exclusion; on rdev it is
    /// the sole in-Rust defense.
    pub(super) injection_guard: Arc<InjectionGuard>,
    /// Concrete OS listener the manager thread is wired to. Stable for the
    /// lifetime of this handle; exposed via [`HotkeyHandle::driver_name`] so
    /// callers (log lines, install envelopes) can surface which backend the
    /// selector picked. `"rdev"` for the X11 / Windows / macOS global hook,
    /// `"evdev"` for the Linux/Wayland `/dev/input` reader.
    pub(super) driver: &'static str,
    /// Push-to-talk ownership state. Holding the lock here is what ties
    /// its lifetime to the installed hotkey: the handle's `Drop` releases
    /// it, so a torn-down subsystem frees push-to-talk for the next
    /// process without any explicit release call.
    ///
    pub(super) ptt_lock: PttOwnershipState,
}

/// Whether this handle is currently reserving the push-to-talk chord.
#[cfg(feature = "rust-hotkeys")]
#[derive(Debug)]
pub(super) enum PttOwnershipState {
    /// This handle owns push-to-talk. No other process can install a
    /// hotkey while this lives.
    Held { _lock: ptt_lock::PttLock },
    /// The guard is not engaged for this handle. Two ways to get here:
    ///
    /// * The lock file could not be opened at install time. That fails
    ///   OPEN by design (see [`ptt_lock`]) and was logged.
    /// * A test stub built by `install_stub_for_tests`, which never goes
    ///   through `install_hotkey`.
    Inactive,
}

#[cfg(feature = "rust-hotkeys")]
impl PttOwnershipState {
    /// True only in [`Self::Held`].
    fn is_held(&self) -> bool {
        matches!(self, Self::Held { .. })
    }

    /// Drop the OS lock now while the handle continues through asynchronous
    /// thread teardown.
    fn release(&mut self) {
        drop(std::mem::replace(self, Self::Inactive));
    }
}

/// Stub handle for builds without the `rust-hotkeys` feature. Exists so
/// the public type-name resolves in error messages on stock builds;
/// constructing one always fails via [`install_hotkey`].
#[cfg(not(feature = "rust-hotkeys"))]
pub struct HotkeyHandle {
    _private: (),
}

#[cfg(feature = "rust-hotkeys")]
impl HotkeyHandle {
    /// Name of the OS listener the manager thread is wired to (`"rdev"` on
    /// X11 / Windows / macOS, `"evdev"` on Linux/Wayland). Used by the
    /// diagnostic `wd hotkey capture` CLI to surface the picked
    /// backend in its `listener_installed` envelope so the operator can tell
    /// at a glance which path fired without needing `VOICEPI_HOTKEY_DEBUG=1`.
    pub fn driver_name(&self) -> &'static str {
        self.driver
    }

    /// Register an optional Windows GUI action without touching PTT state.
    #[cfg(target_os = "windows")]
    pub fn register_copy_last(&self, key_names: Vec<String>) -> std::result::Result<(), String> {
        if self.driver != manager::DRIVER_NAME_REGISTER {
            return Err(
                "copy-last shortcut requires the Windows RegisterHotKey listener".to_owned(),
            );
        }
        self.manager.register_copy_last(key_names)
    }

    /// Register an optional Windows GUI action without touching PTT state.
    #[cfg(target_os = "windows")]
    pub fn register_paste_last(&self, key_names: Vec<String>) -> std::result::Result<(), String> {
        if self.driver != manager::DRIVER_NAME_REGISTER {
            return Err(
                "paste-last shortcut requires the Windows RegisterHotKey listener".to_owned(),
            );
        }
        self.manager.register_paste_last(key_names)
    }

    /// Register the optional cycle-mode shortcut on the RegisterHotKey
    /// listener. Other drivers reject it like the paste-last one does.
    pub fn register_cycle_mode(&self, key_names: Vec<String>) -> std::result::Result<(), String> {
        if self.driver != manager::DRIVER_NAME_REGISTER {
            return Err("mode shortcut requires the Windows RegisterHotKey listener".to_owned());
        }
        self.manager.register_cycle_mode(key_names)
    }

    /// Register the optional raw-mode shortcut on the RegisterHotKey
    /// listener. Other drivers reject it like the paste-last one does.
    pub fn register_raw_mode(&self, key_names: Vec<String>) -> std::result::Result<(), String> {
        if self.driver != manager::DRIVER_NAME_REGISTER {
            return Err("mode shortcut requires the Windows RegisterHotKey listener".to_owned());
        }
        self.manager.register_raw_mode(key_names)
    }

    /// Register the optional clean-mode shortcut on the RegisterHotKey
    /// listener. Other drivers reject it like the paste-last one does.
    pub fn register_clean_mode(&self, key_names: Vec<String>) -> std::result::Result<(), String> {
        if self.driver != manager::DRIVER_NAME_REGISTER {
            return Err("mode shortcut requires the Windows RegisterHotKey listener".to_owned());
        }
        self.manager.register_clean_mode(key_names)
    }

    /// True when this handle carries live push-to-talk ownership
    /// ([`ptt_lock`]) — i.e. no other whisper-dictate process can install
    /// a hotkey while it lives.
    ///
    /// `false` means the ownership lock could not be opened at all and the
    /// install proceeded anyway (the deliberate fail-open path). Worth
    /// surfacing rather than hiding: it is the one configuration in which
    /// the 2026-07-29 double-registration is still reachable, so the
    /// install site logs it.
    pub fn owns_push_to_talk(&self) -> bool {
        self.ptt_lock.is_held()
    }

    /// True while the OS listener thread is still running.
    ///
    /// The rdev driver flips its shared liveness flag to `false` when the
    /// listener thread exits for ANY reason (rdev::listen returned Ok, an
    /// Err quick-failure raced past the ready gate, or a panic unwinds the
    /// closure). Other backends leave the flag at its default `true`, so a
    /// `false` return is an unambiguous "hook is dead" signal but a `true`
    /// return only means "no exit observed", not "guaranteed healthy".
    ///
    /// The boot-self-test uses this to distinguish the exact dead-hook
    /// regression `wd self-test hotkey-boot` was written to
    /// catch: install succeeded, but the listener exited during the hold
    /// window. `listener_exited_early` must not stay hardcoded
    /// `false`, or the self-test emits `ok:true` on that regression.
    pub fn is_listener_alive(&self) -> bool {
        self.manager.is_listener_alive()
    }

    #[cfg(all(test, feature = "rust-injection"))]
    pub(crate) fn mark_listener_dead_for_tests(&self) {
        use std::sync::atomic::Ordering;
        self.manager
            .listener_alive_flag()
            .store(false, Ordering::Relaxed);
    }

    /// Send a [`coordinator::CoordinatorEvent::ProcessingFinished`] for the
    /// given recording id. The host calls this from the transcription
    /// worker when the pass completes so the
    /// [`coordinator::Stage::Processing`] guard releases and the next
    /// press is acted on. The id MUST match the
    /// [`coordinator::CoordinatorAction::StartRecording`] that began the
    /// cycle — a stale id is silently ignored .
    pub fn processing_finished(&self, id: coordinator::RecordingId) {
        self.coordinator
            .send(CoordinatorEvent::ProcessingFinished(id));
    }

    /// Forward a synthetic coordinator event. Used by the integration test
    /// (and rarely the host) to drive specific transitions without going
    /// through the OS listener.
    pub fn send_event(&self, event: CoordinatorEvent) {
        self.coordinator.send(event);
    }

    /// Clone the inner [`coordinator::CoordinatorHandle`].
    ///
    /// The session-backed action sink in
    /// `runtime::rust_session_sink` needs to feed
    /// [`coordinator::CoordinatorEvent::ProcessingFinished`] back into the
    /// coordinator from inside the action callback (after
    /// [`crate::dictate::DictateSession::stop_and_transcribe`] returns).
    /// The closure is constructed BEFORE `install_hotkey` returns -- so the
    /// supervisor populates a shared slot from this accessor after the
    /// install succeeds, and the closure reads it on stop. Lighter-weight
    /// than passing a `Weak<HotkeyHandle>` because the
    /// [`coordinator::CoordinatorHandle`] is already a thin `Clone` wrapper
    /// over the inbound mpsc sender.
    pub fn coordinator_handle(&self) -> CoordinatorHandle {
        self.coordinator.clone()
    }

    /// Clone the shared self-injection guard. The runtime hands the
    /// resulting `Arc` to the injector wrapper
    /// ([`crate::dictate::backends::EnigoInjectBackend::with_injection_guard`])
    /// so the injector can arm the guard around every `SendInput` burst
    /// — the driver callback that already holds an `Arc` clone will
    /// then drop the injected events instead of feeding them into the
    /// tracker (Windows self-injection PTT wedge; see [`inject_guard`]).
    pub fn injection_guard(&self) -> Arc<InjectionGuard> {
        Arc::clone(&self.injection_guard)
    }

    /// Tear the subsystem down cleanly. Idempotent.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    /// Close listener/coordinator inputs without joining their threads.
    ///
    /// The native UI calls this on its event thread, then moves the handle to a
    /// background teardown thread for the potentially long joins. A coordinator
    /// may still be inside synchronous transcription, so joining here would
    /// freeze egui until that request or inference pass returned.
    pub(crate) fn begin_shutdown(&mut self) {
        let _ = self.manager.unregister();
        // `unregister` is synchronous: once it returns, this process no
        // longer owns an active PTT binding. Release cross-process ownership
        // before the coordinator join, which may wait for a local inference
        // or cloud timeout on the background teardown thread.
        self.ptt_lock.release();
        self.manager.shutdown();
        self.coordinator.shutdown();
    }

    /// Windows supervisor test seam: a real manager/coordinator pair without
    /// installing a global OS hook or taking the process-wide PTT lock.
    #[cfg(all(test, feature = "rust-injection"))]
    pub(crate) fn install_stub_for_tests(
        mode: coordinator::Mode,
        key_names: Vec<String>,
    ) -> (
        Self,
        Arc<std::sync::Mutex<crate::hotkey::manager::tracker::KeyTracker>>,
    ) {
        use crate::hotkey::manager::{driver_common, tracker::KeyTracker};
        use std::sync::atomic::Ordering;

        let (manager, cmd_rx) = driver_common::manager_channel();
        manager.listener_alive_flag().store(true, Ordering::Relaxed);
        let tracker = Arc::new(std::sync::Mutex::new(KeyTracker::new(Vec::new())));
        let manager_thread = driver_common::spawn_manager_thread(cmd_rx, Arc::clone(&tracker))
            .expect("stub manager spawns");
        manager
            .register(key_names)
            .expect("stub manager registers initial test binding");
        let options = Options {
            mode,
            auto_complete_processing: true,
        };
        let (coordinator, coordinator_thread) =
            spawn_coordinator(options, |_action| {}, Instant::now);
        let handle = HotkeyHandle {
            coordinator,
            coordinator_thread: Some(coordinator_thread),
            manager,
            manager_thread: Some(manager_thread),
            injection_guard: Arc::new(InjectionGuard::new()),
            driver: "test-stub",
            ptt_lock: PttOwnershipState::Inactive,
        };
        (handle, tracker)
    }

    fn shutdown_inner(&mut self) {
        self.begin_shutdown();
        if let Some(t) = self.coordinator_thread.take() {
            t.join();
        }
        if let Some(t) = self.manager_thread.take() {
            t.join();
        }
    }
}

#[cfg(feature = "rust-hotkeys")]
impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

#[cfg(not(feature = "rust-hotkeys"))]
impl HotkeyHandle {
    /// No-op: the stub handle cannot have anything to shut down.
    pub fn shutdown(self) {}
    /// No-op: the stub handle owns no listener threads.
    pub(crate) fn begin_shutdown(&mut self) {}
    /// Stub build has no listener installed — the CLI/caller shouldn't be
    /// calling this on a stub handle, but returning a constant lets the
    /// call site type-check without a feature-gate at every use.
    pub fn driver_name(&self) -> &'static str {
        "none"
    }
    /// Stub build never installs a hotkey, so it never takes push-to-talk
    /// ownership either. Present so callers need no feature gate.
    pub fn owns_push_to_talk(&self) -> bool {
        false
    }
    /// Stub build never has a live listener; report as dead so a caller
    /// polling this doesn't get a false "alive" reading.
    pub fn is_listener_alive(&self) -> bool {
        false
    }
    /// Stub coordinator handle so `capture::run_capture` can call
    /// `handle.coordinator_handle()` without a feature-gate at every
    /// use — same shape as [`Self::driver_name`]. The stock
    /// [`install_hotkey_with_raw_tap`] always returns
    /// [`InstallError::Unsupported`], so no `HotkeyHandle` value is ever
    /// produced on this path and this method cannot be invoked at runtime;
    /// the returned handle's send channel is disconnected on purpose.
    pub fn coordinator_handle(&self) -> CoordinatorHandle {
        CoordinatorHandle::disconnected()
    }
}

#[cfg(test)]
#[path = "handle_tests.rs"]
mod tests;
