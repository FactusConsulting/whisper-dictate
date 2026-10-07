//! Rust-side push-to-talk hotkey coordinator (issue #318).
//!
//! PTT is owned by Rust and serialises every lifecycle event through a
//! single-threaded stage state machine so cross-process modifier and
//! release races are unrepresentable.
//!
//! The module is gated behind the `rust-hotkeys` cargo feature for the
//! manager / OS-listener layer; the side-aware matching and the stage state
//! machine compile unconditionally so their unit tests run on every CI job.
//! Activation at runtime needs both:
//!
//! * a binary built with `--features rust-hotkeys`, and
//! * the env var `VOICEPI_HOTKEY_BACKEND=rust`.
//!
//! Without either, startup fails with an actionable feature-build message;
//! there is no listener fallback.
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
//! recording id) back into the coordinator when transcription completes
//! that is what releases the [`coordinator::Stage::Processing`] guard so
//! the next press is acted on.
//!
//! ## Public API
//!
//! Most consumers only need [`install_hotkey`] / [`HotkeyHandle`]. The inner
//! modules are `pub` for the unit tests and the integration test (which
//! drives the coordinator and tracker directly with synthetic events).

pub mod boot_self_test;

// Companion tests for `boot_self_test.rs`. Split out of an inline
// `#[cfg(test)] mod tests` so the regression-test discipline scanner
// sees a matching test file next to the production module — same
// pattern as `manager/rdev_driver_tests.rs`.
#[cfg(test)]
#[path = "boot_self_test_tests.rs"]
mod boot_self_test_tests;

pub mod capture;
#[cfg(test)]
#[path = "capture_tests.rs"]
mod capture_tests;
pub mod coordinator;
pub mod inject_guard;

// Companion tests for `inject_guard.rs`, following the same scanner
// pattern as `boot_self_test_tests` above: helpers used only by tests
// must live in a scanner-visible `*_tests.rs` file.
#[cfg(test)]
#[path = "inject_guard_tests.rs"]
mod inject_guard_tests;

// Companion tests for the tracker-bridge decision ([`bridge_decision`]).
// Gated like the bridge itself: the sinks struct only exists with the
// rust-hotkeys feature, so Linux / stock builds compile the module to
// nothing and the feature-gated CI legs run it.
#[cfg(all(test, feature = "rust-hotkeys"))]
#[path = "hotkey_bridge_tests.rs"]
mod hotkey_bridge_tests;

pub mod manager;
pub mod modifier_match;

// Process-bound "only one whisper-dictate owns push-to-talk" guard.
// Compiled unconditionally (it is pure `std`) so its tests run on every
// CI leg, not only the `rust-hotkeys` one -- the guard's whole value is
// that it sits ABOVE driver selection, so tying it to a driver feature
// would be exactly the wrong coupling. Wired into
// [`install_hotkey_with_raw_tap`] below.
pub mod ptt_lock;

pub mod self_test;

// Companion tests for this file. Structural scanners that pin the
// ownership guard's wiring into `install_hotkey_with_raw_tap` -- an
// invariant with no behavioural seam, since a real install needs an OS
// listener headless CI cannot provide. Split into its own file so the
// regression-test discipline scanner sees a companion next to `mod.rs`.
#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;

mod config;
mod handle;
mod install;
mod preflight;

pub use inject_guard::{InjectionBracket, InjectionGuard};

pub use config::Result;
pub use config::{HotkeyConfig, HotkeyPreflight, InstallError};
pub use handle::HotkeyHandle;
pub use install::install_hotkey;
#[cfg(feature = "rust-hotkeys")]
pub use install::install_hotkey_with_actions;
#[cfg(feature = "rust-hotkeys")]
pub use install::install_hotkey_with_copy_last;
pub(crate) use install::install_hotkey_with_focus_snapshot;
pub use install::install_hotkey_with_raw_tap;
#[cfg(feature = "rust-hotkeys")]
pub use install::HotkeyActionSinks;
pub use preflight::{
    preflight_hotkey, rust_hotkey_backend_available, rust_hotkey_backend_requested,
    validate_key_names,
};

#[cfg(all(test, feature = "rust-hotkeys"))]
mod integration {
    use super::coordinator::{CoordinatorAction, CoordinatorEvent};
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
        // `install_hotkey` publishes a fresh `InjectionGuard` into the
        // process-global slot via `inject_guard::set_global` BEFORE it
        // attempts listener startup — so this test mutates a
        // process-global singleton even on the headless path where the
        // install later fails. Hold the crate-wide guard lock so it
        // cannot race `inject_guard_tests::global_slot_last_writer_wins`,
        // whose `Arc::ptr_eq` assertions would otherwise see this
        // test's guard replace theirs mid-assertion.

        let _guard_lock = crate::test_env_lock::GLOBAL_GUARD_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // `install_hotkey` also touches the process-global push-to-talk
        // refusal slot: it `clear()`s it on entry and may `record()` a
        // conflict. The report / UI-banner tests take this same lock
        // around their own `record()` -> `current()` pairs, so without it
        // here a parallel run of this test could clear their fixture
        // between the two calls and fail them intermittently. Taken in
        // the same order as GLOBAL_GUARD_LOCK everywhere that needs both,
        // so the pair cannot deadlock.
        let _slot_lock = ptt_lock::report::TEST_SLOT_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let (tx, rx) = mpsc::channel();
        let cfg = HotkeyConfig::hold_to_talk(vec!["ctrl_l".to_owned()]);
        let handle = match install_hotkey(cfg, move |action| {
            tx.send(action).expect("test channel open");
        }) {
            Ok(h) => h,
            Err(InstallError::ListenerStartup(_)) => {
                // Headless env (CI container, missing macOS accessibility
                // permission, ...) — the install correctly refused to take
                // over because the platform driver couldn't start. That's
                // the expected "not applicable" path on this platform
                // rather than a failure.
                eprintln!(
                    "skipping install_then_drive_coordinator_emits_actions_in_order: \
                     rdev listener refused to start (headless env)"
                );
                return;
            }
            Err(InstallError::AlreadyHeld { holder_pid, .. }) => {
                // A developer running this suite with the tray app open.
                // The guard is doing exactly its job (`hotkey::ptt_lock`),
                // so this is "not applicable on this box" rather than a
                // failure -- same shape as the headless arm above. CI has
                // no other whisper-dictate process, so the test still runs
                // there.
                eprintln!(
                    "skipping install_then_drive_coordinator_emits_actions_in_order: \
                     push-to-talk is owned by another whisper-dictate process \
                     (pid {holder_pid:?})"
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
        // configs with names the rdev driver can't translate must
        // be rejected synchronously so a replacement listener is never
        // attempted with a binding that cannot fire.
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

    /// Pins the feature-build behaviour of `validate_key_names`, the
    /// pure helper the runtime restart path consults before replacing the
    /// listener on a key-binding change.
    /// Empty input must surface `EmptyConfig`; an unsupported name must
    /// surface `UnsupportedKey(name)` and stop at the FIRST bad name in
    /// the list (so the message is deterministic). The all-supported
    /// path returns Ok so the restart gate can replace the listener.
    #[test]
    fn validate_key_names_feature_build_matches_install_validation() {
        assert!(matches!(
            validate_key_names(&[]),
            Err(InstallError::EmptyConfig)
        ));
        // First-bad-name is reported (deterministic for error UX).
        match validate_key_names(&["ctrl_l".to_owned(), "super_l".to_owned()]) {
            Err(InstallError::UnsupportedKey(name)) => assert_eq!(name, "super_l"),
            other => {
                panic!("expected UnsupportedKey(\"super_l\") for first-bad-name, got: {other:?}")
            }
        }
        match validate_key_names(&["super_l".to_owned(), "ctrl_l".to_owned()]) {
            Err(InstallError::UnsupportedKey(name)) => assert_eq!(name, "super_l"),
            other => panic!("expected UnsupportedKey(\"super_l\") first, got: {other:?}"),
        }
        // All-supported -> Ok so the restart path can replace the listener.
        assert!(validate_key_names(&["ctrl_l".to_owned(), "f9".to_owned()]).is_ok());
    }
}
