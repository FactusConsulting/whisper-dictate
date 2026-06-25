//! Rust-side push-to-talk hotkey coordinator (issue #318).
//!
//! Today PTT lives in [`vp_keys.py`](../../python/whisper_dictate/vp_keys.py) /
//! [`vp_keys_solo.py`](../../python/whisper_dictate/vp_keys_solo.py) on top of
//! pynput/evdev, and lifecycle events cross a Python→Rust IPC with imperfect
//! modifier matching. That has produced recurring race-condition bugs (#254,
//! #274). This module moves the hotkey loop into Rust and serialises every
//! lifecycle event through a single-threaded stage state machine so the whole
//! class of races becomes unrepresentable.
//!
//! The module is gated behind the `rust-hotkeys` cargo feature for the
//! manager / OS-listener layer; the side-aware matching and the stage state
//! machine compile unconditionally so their unit tests run on every CI job.
//! Activation at runtime needs both:
//!
//! * a binary built with `--features rust-hotkeys`, and
//! * the env var `VOICEPI_HOTKEY_BACKEND=rust`.
//!
//! Without either, the supervisor's behaviour is byte-identical to today —
//! pynput stays the shipping path for this PR.
//!
//! ## Architecture
//!
//! ```text
//!  OS key events ──▶ rdev listener (manager/rdev_driver.rs, feature-gated)
//!                       │
//!                       ▼
//!                  KeyTracker (manager/tracker.rs, side-aware press/release/cancel)
//!                       │  TrackerOutput { ChordPress | ChordRelease | ChordCancel }
//!                       ▼
//!                  CoordinatorHandle::send (coordinator/mod.rs)
//!                       │
//!                       ▼
//!                  TranscriptionCoordinator thread
//!                       │  Stage state machine + 30 ms press debounce
//!                       ▼
//!                  CoordinatorAction { StartRecording | StopAndTranscribe | CancelRecording }
//!                       │
//!                       ▼
//!                  Host action sink (runtime.rs: starts/stops the worker)
//! ```
//!
//! The host is also responsible for sending
//! [`coordinator::CoordinatorEvent::ProcessingFinished`] (with the matching
//! recording id) back into the coordinator when transcription completes —
//! that is what releases the [`coordinator::Stage::Processing`] guard so
//! the next press is acted on.
//!
//! ## Public API
//!
//! Most consumers only need [`install_hotkey`] / [`HotkeyHandle`]. The inner
//! modules are `pub` for the unit tests and the integration test (which
//! drives the coordinator and tracker directly with synthetic events).

pub mod coordinator;
pub mod manager;
pub mod modifier_match;

#[cfg(feature = "rust-hotkeys")]
use std::time::Instant;

use coordinator::Mode;

#[cfg(feature = "rust-hotkeys")]
use coordinator::{
    spawn as spawn_coordinator, CoordinatorAction, CoordinatorEvent, CoordinatorHandle,
    CoordinatorThread, Options,
};
#[cfg(feature = "rust-hotkeys")]
use manager::{
    is_rdev_supported_name, spawn as spawn_manager, ManagerHandle, ManagerThread, SpawnError,
    TrackerOutput,
};

/// User-facing configuration for the Rust hotkey backend.
///
/// `key_names` is the PTT setting `key` split on `+`, with names matching
/// the Python convention (`ctrl_l`, `shift_r`, `alt_gr`, `f9`, ...). An
/// empty vector is a configuration error and will be rejected by
/// [`install_hotkey`]; names the rdev driver cannot translate are rejected
/// with [`InstallError::UnsupportedKey`] so a misconfiguration cannot
/// silently park the Python listener for keys that will never fire.
///
/// `mode` selects hold-to-talk (default) or toggle behaviour. It must be
/// captured by the supervisor from the same `VOICEPI_TOGGLE` / config
/// source the Python listener reads so both backends behave identically.
#[derive(Debug, Clone)]
pub struct HotkeyConfig {
    pub key_names: Vec<String>,
    pub mode: Mode,
}

impl HotkeyConfig {
    /// Build a hold-to-talk config from a list of key names. Convenience
    /// for the common case (matches the historical constructor signature
    /// from before the toggle-mode field landed).
    pub fn hold_to_talk(key_names: Vec<String>) -> Self {
        Self {
            key_names,
            mode: Mode::HoldToTalk,
        }
    }
}

/// Owning handle for the Rust hotkey subsystem. Drop or
/// [`HotkeyHandle::shutdown`] to tear it all down. The OS listener thread
/// itself cannot be interrupted (rdev limitation), so a tear-down leaks
/// one thread until process exit — acceptable because the supervisor
/// installs the hotkey subsystem once per process.
#[cfg(feature = "rust-hotkeys")]
pub struct HotkeyHandle {
    coordinator: CoordinatorHandle,
    coordinator_thread: Option<CoordinatorThread>,
    manager: ManagerHandle,
    manager_thread: Option<ManagerThread>,
}

/// Stub handle for builds without the `rust-hotkeys` feature. Exists so
/// the public type-name resolves in error messages on stock builds;
/// constructing one always fails via [`install_hotkey`].
#[cfg(not(feature = "rust-hotkeys"))]
pub struct HotkeyHandle {
    _private: (),
}

/// Errors from [`install_hotkey`].
///
/// * [`Self::Unsupported`] — the binary was built without the
///   `rust-hotkeys` cargo feature. The supervisor logs a warning and stays
///   on the pynput path.
/// * [`Self::EmptyConfig`] — the PTT binding came in empty.
/// * [`Self::UnsupportedKey`] — a configured key name has no rdev
///   translation (e.g. `super_l`, which the Python evdev backend accepts
///   but rdev does not). Surfaced BEFORE the supervisor disables Python so
///   it can keep the pynput path wired (P2 #6).
/// * [`Self::ListenerStartup`] — `rdev::listen` failed at startup (no X
///   display, missing accessibility permission, ...). Surfaced
///   synchronously so the supervisor can fall back to pynput (P1 #2).
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("rust-hotkeys feature is not compiled in (rebuild with --features rust-hotkeys)")]
    Unsupported,
    #[error("hotkey config is empty (key_names cannot be empty)")]
    EmptyConfig,
    #[error("hotkey key name {0:?} is not supported by the Rust (rdev) backend")]
    UnsupportedKey(String),
    #[error("rdev listener failed to start: {0}")]
    ListenerStartup(String),
}

/// Convenience alias for the install API.
pub type Result<T> = std::result::Result<T, InstallError>;

/// Install the Rust hotkey subsystem with the given configuration.
///
/// On success the returned [`HotkeyHandle`] keeps the manager + coordinator
/// threads alive until [`HotkeyHandle::shutdown`] is called (or it is
/// dropped — `Drop` also shuts down cleanly).
///
/// `action_sink` is invoked on the coordinator thread for every action it
/// emits ([`coordinator::CoordinatorAction`]) — the host wires this up to
/// its existing start/stop hooks. It MUST be cheap and non-blocking; spawn
/// a worker thread if you need to do real work.
///
/// On a stock build (no `rust-hotkeys` feature) this always returns
/// [`InstallError::Unsupported`] so the supervisor can fall back to pynput.
///
/// All configuration-side errors are surfaced BEFORE the manager or
/// coordinator threads are spawned, and the rdev listener startup error
/// (the platform doesn't allow the global hook) is surfaced synchronously
/// so the caller can keep Python wired if Rust can't take over.
#[cfg(feature = "rust-hotkeys")]
pub fn install_hotkey<F>(config: HotkeyConfig, action_sink: F) -> Result<HotkeyHandle>
where
    F: FnMut(CoordinatorAction) + Send + 'static,
{
    if config.key_names.is_empty() {
        return Err(InstallError::EmptyConfig);
    }
    // Reject names rdev cannot translate BEFORE we spawn anything. Without
    // this the install would succeed but every press would be silently
    // dropped — and worse, the supervisor would have disabled the Python
    // listener for a binding that can never fire (P2 #6).
    for name in &config.key_names {
        if !is_rdev_supported_name(name) {
            return Err(InstallError::UnsupportedKey(name.clone()));
        }
    }

    let options = Options { mode: config.mode };
    let (coord_handle, coord_thread) = spawn_coordinator(options, action_sink, Instant::now);

    // Bridge: TrackerOutput → CoordinatorEvent. Cloneable handle so the
    // closure captures a Sender that's cheap to call from the rdev callback.
    let bridge = coord_handle.clone();
    let (mgr_handle, mgr_thread) = match spawn_manager(move |out| {
        let event = match out {
            TrackerOutput::ChordPress => CoordinatorEvent::Press,
            TrackerOutput::ChordRelease => CoordinatorEvent::Release,
            TrackerOutput::ChordCancel => CoordinatorEvent::Cancel,
        };
        bridge.send(event);
    }) {
        Ok(pair) => pair,
        Err(err) => {
            // Listener (or manager-thread) startup failed. Tear the
            // coordinator down so we don't leak the thread, and surface
            // the error to the supervisor so it can keep Python wired
            // (P1 #2).
            coord_handle.shutdown();
            coord_thread.join();
            return Err(InstallError::ListenerStartup(spawn_err_message(err)));
        }
    };

    if let Err(err) = mgr_handle.register(config.key_names.clone()) {
        eprintln!("[hotkey] failed to register Rust hotkey binding: {err}");
        // Best-effort cleanup — both threads were spawned. Map the
        // register failure to ListenerStartup since at this point we DID
        // get past listener init and the failure is in the control channel.
        mgr_handle.shutdown();
        coord_handle.shutdown();
        mgr_thread.join();
        coord_thread.join();
        return Err(InstallError::ListenerStartup(err));
    }

    Ok(HotkeyHandle {
        coordinator: coord_handle,
        coordinator_thread: Some(coord_thread),
        manager: mgr_handle,
        manager_thread: Some(mgr_thread),
    })
}

/// Stringify a [`SpawnError`] for the [`InstallError::ListenerStartup`]
/// payload (the variants carry their own messages — this just normalises
/// the `ListenerHung` arm).
#[cfg(feature = "rust-hotkeys")]
fn spawn_err_message(e: SpawnError) -> String {
    match e {
        SpawnError::ListenerStartup(msg) => msg,
        SpawnError::ListenerHung => "listener thread did not report readiness".to_owned(),
    }
}

/// Stub `install_hotkey` for builds without the feature. Always returns
/// [`InstallError::Unsupported`] — the supervisor's contract is to log a
/// warning and stay on the pynput path in that case.
#[cfg(not(feature = "rust-hotkeys"))]
pub fn install_hotkey<F>(_config: HotkeyConfig, _action_sink: F) -> Result<HotkeyHandle>
where
    F: FnMut(coordinator::CoordinatorAction) + Send + 'static,
{
    Err(InstallError::Unsupported)
}

#[cfg(feature = "rust-hotkeys")]
impl HotkeyHandle {
    /// Send a [`coordinator::CoordinatorEvent::ProcessingFinished`] for the
    /// given recording id. The host calls this from the transcription
    /// worker when the pass completes so the
    /// [`coordinator::Stage::Processing`] guard releases and the next
    /// press is acted on. The id MUST match the
    /// [`coordinator::CoordinatorAction::StartRecording`] that began the
    /// cycle — a stale id is silently ignored (P2 #9).
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

    /// Tear the subsystem down cleanly. Idempotent.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        let _ = self.manager.unregister();
        self.manager.shutdown();
        self.coordinator.shutdown();
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
}

/// Has the user requested the Rust hotkey backend via env var? Pure helper
/// (no side effects) so the gate is unit-testable without spawning threads.
/// Returns false for unset / empty / any non-`rust` value.
pub fn rust_hotkey_backend_requested() -> bool {
    std::env::var("VOICEPI_HOTKEY_BACKEND")
        .map(|v| v.trim().eq_ignore_ascii_case("rust"))
        .unwrap_or(false)
}

/// Whether the running binary can actually serve the request. The Rust
/// hotkey loop is gated behind the `rust-hotkeys` cargo feature, so a
/// stock build returns false even if the env var is set. The supervisor
/// logs a one-line warning and stays on the pynput path in that case so
/// the user is never silently surprised.
pub fn rust_hotkey_backend_available() -> bool {
    cfg!(feature = "rust-hotkeys")
}

#[cfg(all(test, feature = "rust-hotkeys"))]
mod integration {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// End-to-end coordinator wiring test: install the hotkey subsystem,
    /// drive the coordinator with synthetic CoordinatorEvents (skipping the
    /// rdev layer which we cannot inject real OS events into from a test),
    /// and assert the action sink sees the expected stage transitions.
    ///
    /// The tracker integration is covered by `tracker_tests` in
    /// `manager.rs`; the rdev driver is unjoinable so we don't smoke-test
    /// it here — every other layer between OS events and the action sink
    /// IS exercised.
    #[test]
    fn install_then_drive_coordinator_emits_actions_in_order() {
        let (tx, rx) = mpsc::channel();
        let cfg = HotkeyConfig::hold_to_talk(vec!["ctrl_l".to_owned()]);
        let handle = match install_hotkey(cfg, move |action| {
            tx.send(action).expect("test channel open");
        }) {
            Ok(h) => h,
            Err(InstallError::ListenerStartup(_)) => {
                // Headless env (CI container, missing macOS accessibility
                // permission, ...) — the install correctly refused to park
                // Python because Rust couldn't take over. That's exactly
                // the P1 #2 path, so we treat it as "not applicable" on
                // this platform rather than fail.
                eprintln!(
                    "skipping install_then_drive_coordinator_emits_actions_in_order: \
                     rdev listener refused to start (headless env)"
                );
                return;
            }
            Err(other) => panic!("install: {other:?}"),
        };

        handle.send_event(CoordinatorEvent::Press);
        let first = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("StartRecording action");
        let id = match first {
            CoordinatorAction::StartRecording(id) => id,
            other => panic!("expected StartRecording, got {other:?}"),
        };

        handle.send_event(CoordinatorEvent::Release);
        let second = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("StopAndTranscribe action");
        assert!(matches!(second, CoordinatorAction::StopAndTranscribe(_)));

        handle.processing_finished(id);
        // No action emitted for ProcessingFinished; just confirm no spurious
        // action shows up.
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());

        handle.shutdown();
    }

    #[test]
    fn empty_config_is_rejected() {
        let cfg = HotkeyConfig::hold_to_talk(Vec::new());
        // Don't use expect_err — HotkeyHandle doesn't implement Debug (it
        // owns thread join handles + channel senders, none Debug-able) and
        // we don't want to give it one just to satisfy the test runner.
        let err = match install_hotkey(cfg, |_| {}) {
            Ok(_) => panic!("expected EmptyConfig error, got Ok"),
            Err(e) => e,
        };
        assert!(matches!(err, InstallError::EmptyConfig));
    }

    #[test]
    fn unsupported_key_is_rejected_up_front() {
        // P2 #6: configs with names the rdev driver can't translate must
        // be rejected synchronously so the supervisor never disables Python
        // for a binding that will never fire. `super_l` is accepted by the
        // Python evdev backend but not by our rdev key map.
        let cfg = HotkeyConfig::hold_to_talk(vec!["super_l".to_owned()]);
        let err = match install_hotkey(cfg, |_| {}) {
            Ok(_) => panic!("expected UnsupportedKey error, got Ok"),
            Err(e) => e,
        };
        match err {
            InstallError::UnsupportedKey(name) => assert_eq!(name, "super_l"),
            other => panic!("expected UnsupportedKey, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod env_tests {
    use super::*;

    #[test]
    fn backend_requested_reads_env_var_truthy_rust_only() {
        // Hold the crate-wide env lock so we don't race other env-mutating
        // tests in the same binary — see crate::test_env_lock for the
        // soundness contract.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("VOICEPI_HOTKEY_BACKEND").ok();

        std::env::remove_var("VOICEPI_HOTKEY_BACKEND");
        assert!(!rust_hotkey_backend_requested());

        std::env::set_var("VOICEPI_HOTKEY_BACKEND", "rust");
        assert!(rust_hotkey_backend_requested());

        std::env::set_var("VOICEPI_HOTKEY_BACKEND", "RUST");
        assert!(rust_hotkey_backend_requested());

        std::env::set_var("VOICEPI_HOTKEY_BACKEND", "pynput");
        assert!(!rust_hotkey_backend_requested());

        std::env::set_var("VOICEPI_HOTKEY_BACKEND", "");
        assert!(!rust_hotkey_backend_requested());

        match prev {
            Some(v) => std::env::set_var("VOICEPI_HOTKEY_BACKEND", v),
            None => std::env::remove_var("VOICEPI_HOTKEY_BACKEND"),
        }
    }
}
