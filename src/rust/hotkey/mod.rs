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

pub mod boot_self_test;

// Companion tests for `boot_self_test.rs`. Split out of an inline
// `#[cfg(test)] mod tests` so the regression-test discipline scanner
// sees a matching test file next to the production module — same
// pattern as `manager/rdev_driver_tests.rs`. See
// `src/tests/python/test_regression_test_discipline.py`.
#[cfg(test)]
#[path = "boot_self_test_tests.rs"]
mod boot_self_test_tests;

pub mod capture;
pub mod coordinator;
pub mod inject_guard;

// Companion tests for `inject_guard.rs`. Split out of an inline
// `#[cfg(test)] mod tests` for the same scanner reason as
// `boot_self_test_tests` above — the sonar gate on PR #668 flagged
// `clear_global_for_tests` (added by the last-writer-wins `set_global`
// fix, Codex P2 #668 discussion 3665741347) as an untested new symbol
// while its tests were still inline.
#[cfg(test)]
#[path = "inject_guard_tests.rs"]
mod inject_guard_tests;

pub mod manager;
pub mod modifier_match;
pub mod self_test;

#[cfg(feature = "rust-hotkeys")]
use std::sync::Arc;
#[cfg(feature = "rust-hotkeys")]
use std::time::Instant;

use coordinator::{CoordinatorHandle, Mode};

#[cfg(feature = "rust-hotkeys")]
use coordinator::{
    spawn as spawn_coordinator, CoordinatorAction, CoordinatorEvent, CoordinatorThread, Options,
};
pub use inject_guard::{InjectionBracket, InjectionGuard};
#[cfg(feature = "rust-hotkeys")]
use manager::{
    driver_from_env, is_rdev_supported_name, spawn_with_driver as spawn_manager_with_driver,
    DriverKind, ManagerHandle, ManagerThread, RawTap, SpawnError, TrackerOutput,
};

/// Alias for the `(driver_name, ManagerHandle, ManagerThread)` triple that
/// [`manager::spawn_with_raw_tap`] returns. Named so the docs elsewhere in
/// this module can refer to the tuple without inlining its full type.
#[cfg(feature = "rust-hotkeys")]
type SpawnManagerOk = (&'static str, ManagerHandle, ManagerThread);

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
    /// Forwarded to [`coordinator::Options::auto_complete_processing`]. The
    /// diagnostic `hotkey capture` CLI sets this so it doesn't need a
    /// per-cycle `ProcessingFinished` from the sink — the coordinator
    /// completes the stage synchronously as part of the same loop iteration
    /// that emitted `StopAndTranscribe`, before any already-queued next
    /// press is consumed. The shipping supervisor leaves it `false`.
    pub auto_complete_processing: bool,
}

impl HotkeyConfig {
    /// Build a hold-to-talk config from a list of key names. Convenience
    /// for the common case (matches the historical constructor signature
    /// from before the toggle-mode field landed).
    pub fn hold_to_talk(key_names: Vec<String>) -> Self {
        Self {
            key_names,
            mode: Mode::HoldToTalk,
            auto_complete_processing: false,
        }
    }

    /// Fluent setter for [`Self::auto_complete_processing`]. Used by the
    /// diagnostic capture CLI; keeps `HotkeyConfig::hold_to_talk`'s
    /// signature unchanged.
    pub fn with_auto_complete_processing(mut self, on: bool) -> Self {
        self.auto_complete_processing = on;
        self
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
    /// Shared self-injection guard. The driver callback reads this to
    /// drop OS events that the app's own text injector is currently
    /// producing (Windows self-injection PTT wedge; see
    /// [`inject_guard`] for the full rationale). Exposed via
    /// [`HotkeyHandle::injection_guard`] so the runtime can hand the
    /// same handle to the injector — arms on the injector side, checks
    /// on the driver side. On the evdev backend it is the belt-and-braces
    /// second layer behind device-enumeration exclusion; on rdev it is
    /// the sole in-Rust defense.
    injection_guard: Arc<InjectionGuard>,
    /// Concrete OS listener the manager thread is wired to. Stable for the
    /// lifetime of this handle; exposed via [`HotkeyHandle::driver_name`] so
    /// callers (log lines, install envelopes) can surface which backend the
    /// selector picked. `"rdev"` for the X11 / Windows / macOS global hook,
    /// `"evdev"` for the Linux/Wayland `/dev/input` reader.
    driver: &'static str,
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
    install_hotkey_with_raw_tap(config, action_sink, manager::NoopRawTap)
}

/// Same as [`install_hotkey`] but also invokes `raw_tap` for every OS key
/// event the rdev listener translates, BEFORE the tracker processes it. The
/// diagnostic `whisper-dictate hotkey capture` CLI uses this so the operator
/// can see individual keydown/keyup events alongside the chord-level actions.
///
/// The tap runs on the rdev listener thread; keep it cheap and non-blocking.
/// All the validation / startup-failure semantics from [`install_hotkey`]
/// apply unchanged — this function is a thin generalisation.
#[cfg(feature = "rust-hotkeys")]
pub fn install_hotkey_with_raw_tap<F, R>(
    config: HotkeyConfig,
    action_sink: F,
    raw_tap: R,
) -> Result<HotkeyHandle>
where
    F: FnMut(CoordinatorAction) + Send + 'static,
    R: RawTap,
{
    if config.key_names.is_empty() {
        return Err(InstallError::EmptyConfig);
    }
    // Resolve the driver early so the validator can consult the correct
    // supported-name table. The Windows Register driver accepts a wider
    // set (VK_ virtual keys) than rdev's hand-rolled table — e.g. `pause`
    // has no rdev name but has VK_PAUSE — and skipping the rdev-side
    // validation for that driver lets a GUI user with `pause` install
    // successfully instead of getting rejected up-front.
    let driver_kind = resolve_driver_kind_for_install(&config.key_names);
    match driver_kind {
        #[cfg(target_os = "windows")]
        DriverKind::Register => {
            // Parse via the register driver's own validator. On the fallback
            // path (`resolve_driver_kind_for_install` downgraded to Rdev
            // because parse_chord already failed), we don't reach this arm —
            // the rdev branch below runs instead. If parse_chord fails
            // here we surface the actionable message from the parser.
            if let Err(msg) = manager::win_registerhotkey::parse_chord(&config.key_names) {
                return Err(InstallError::UnsupportedKey(msg));
            }
        }
        _ => {
            // Reject names rdev cannot translate BEFORE we spawn anything.
            // Without this the install would succeed but every press would
            // be silently dropped — and worse, the supervisor would have
            // disabled the Python listener for a binding that can never
            // fire (P2 #6).
            for name in &config.key_names {
                if !is_rdev_supported_name(name) {
                    #[cfg(target_os = "windows")]
                    crate::diag::log!(
                        "[hotkey] chord key {:?} is not in the rdev name \
                         table; consider setting VOICEPI_HOTKEY_DRIVER=register \
                         to use the RegisterHotKey backend which supports a \
                         wider set of virtual keys (VK_PAUSE, VK_HOME, ...).",
                        name
                    );
                    return Err(InstallError::UnsupportedKey(name.clone()));
                }
            }
        }
    }

    let options = Options {
        mode: config.mode,
        auto_complete_processing: config.auto_complete_processing,
    };
    let (coord_handle, coord_thread) = spawn_coordinator(options, action_sink, Instant::now);

    // Shared self-injection guard — armed by the injector wrapper around
    // every SendInput burst, checked by the driver callback below. See
    // [`inject_guard`] for the full rationale (Windows PTT wedge, same
    // class as #467 on Linux/Wayland but with no /dev/input equivalent).
    //
    // Also publish it to the process-wide slot so the runtime's
    // `EnigoInjectBackend` (which was constructed BEFORE this call by
    // the session builder) can pick it up on the next `inject()` call
    // without needing per-layer wiring through the sink builder chain.
    // Same-process second install (test hosts) is a no-op — first
    // writer wins; production `install_hotkey` runs exactly once.
    let injection_guard = Arc::new(InjectionGuard::new());
    inject_guard::set_global(Arc::clone(&injection_guard));

    // Bridge: TrackerOutput → CoordinatorEvent. Cloneable handle so the
    // closure captures a Sender that's cheap to call from the OS listener
    // callback (rdev on the LL-hook thread, evdev on the per-device
    // reader thread).
    let bridge = coord_handle.clone();

    // `driver_kind` was resolved above (before validation) so the same
    // decision drives BOTH the supported-name check and the spawn call.
    let (driver, mgr_handle, mgr_thread): SpawnManagerOk = match spawn_manager_with_driver(
        driver_kind,
        Arc::clone(&injection_guard),
        move |out| {
            let event = match out {
                TrackerOutput::ChordPress => CoordinatorEvent::Press,
                TrackerOutput::ChordRelease => CoordinatorEvent::Release,
                TrackerOutput::ChordCancel => CoordinatorEvent::Cancel,
            };
            bridge.send(event);
        },
        raw_tap,
    ) {
        Ok(triple) => triple,
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
        injection_guard,
        driver,
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

/// Resolve the driver kind for a fresh install, applying the Windows-
/// specific `Register → Rdev` chord-validity fallback.
///
/// The chord is parsed by [`manager::win_registerhotkey::parse_chord`]
/// BEFORE any thread spawns, so a modifier-only binding (unsupported
/// by RegisterHotKey) downgrades to rdev with a diagnostic-log line
/// instead of failing the install. The GUI binary defaults to
/// `Register` (set in `whisper-dictate-gui::main`); this fallback
/// exists so a user with a bare-`ctrl_l` binding does NOT lose PTT
/// because the RegisterHotKey backend can't express the chord.
///
/// On non-Windows targets, or for any non-Register selection, this is
/// a straight `driver_from_env()` pass-through.
#[cfg(feature = "rust-hotkeys")]
fn resolve_driver_kind_for_install(key_names: &[String]) -> DriverKind {
    let kind = driver_from_env();
    #[cfg(target_os = "windows")]
    {
        if kind == DriverKind::Register {
            if let Err(msg) = manager::win_registerhotkey::parse_chord(key_names) {
                crate::diag::log!(
                    "[hotkey] VOICEPI_HOTKEY_DRIVER=register cannot express \
                     the configured chord ({msg}); falling back to rdev for \
                     this install so PTT still works (set \
                     VOICEPI_HOTKEY_DRIVER=rdev in the environment to make \
                     this the pinned default and silence this line)."
                );
                return DriverKind::Rdev;
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = key_names; // silence unused-var warning on non-Windows
    }
    kind
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

/// Stub `install_hotkey_with_raw_tap` for builds without the feature. The
/// `raw_tap` bound is intentionally erased to a no-op closure signature so
/// callers (notably the diagnostic `hotkey capture` CLI) can share the same
/// call shape across feature configurations.
#[cfg(not(feature = "rust-hotkeys"))]
pub fn install_hotkey_with_raw_tap<F, R>(
    _config: HotkeyConfig,
    _action_sink: F,
    _raw_tap: R,
) -> Result<HotkeyHandle>
where
    F: FnMut(coordinator::CoordinatorAction) + Send + 'static,
    R: Send + Sync + 'static,
{
    Err(InstallError::Unsupported)
}

#[cfg(feature = "rust-hotkeys")]
/// Validate `key_names` against the Rust (rdev) backend's supported list
/// WITHOUT installing anything. Mirrors the validation `install_hotkey`
/// runs before spawning manager / coordinator threads, so the supervisor
/// can check a (possibly updated) restart-time binding before parking
/// the Python listener. Returns Err with the first unsupported name.
///
/// Codex P2 #416 (round 2) runtime.rs:511 -- on the restart path the
/// supervisor parks Python BEFORE calling `handle.resume(key_names)`;
/// without this validation a Settings change to a key that the Python
/// evdev backend accepts but rdev does not (eg `super_l`) would leave
/// Python disabled and the Rust hotkey unable to fire.
///
/// In stub builds (no `rust-hotkeys` feature) this always succeeds --
/// `install_hotkey` already returns `Unsupported` synchronously there,
/// so the supervisor's `hotkey_handle` is None and this validation is
/// dead code anyway.
#[cfg(feature = "rust-hotkeys")]
pub fn validate_key_names(key_names: &[String]) -> Result<()> {
    if key_names.is_empty() {
        return Err(InstallError::EmptyConfig);
    }
    for name in key_names {
        if !is_rdev_supported_name(name) {
            return Err(InstallError::UnsupportedKey(name.clone()));
        }
    }
    Ok(())
}

#[cfg(not(feature = "rust-hotkeys"))]
pub fn validate_key_names(_key_names: &[String]) -> Result<()> {
    Ok(())
}

#[cfg(feature = "rust-hotkeys")]
impl HotkeyHandle {
    /// Name of the OS listener the manager thread is wired to (`"rdev"` on
    /// X11 / Windows / macOS, `"evdev"` on Linux/Wayland). Used by the
    /// diagnostic `whisper-dictate hotkey capture` CLI to surface the picked
    /// backend in its `listener_installed` envelope so the operator can tell
    /// at a glance which path fired without needing `VOICEPI_HOTKEY_DEBUG=1`.
    pub fn driver_name(&self) -> &'static str {
        self.driver
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
    /// regression `whisper-dictate self-test hotkey-boot` was written to
    /// catch: install succeeded, but the listener exited during the hold
    /// window. Before this signal `listener_exited_early` was hardcoded
    /// `false`, so the self-test would emit `ok:true` on the regression
    /// (Codex P1 #644 discussion r3658983542).
    pub fn is_listener_alive(&self) -> bool {
        self.manager.is_listener_alive()
    }

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

    /// Clone the inner [`coordinator::CoordinatorHandle`].
    ///
    /// Wave 5 PR 4 of #348: the session-backed action sink in
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

    /// Suspend key tracking: unregister the PTT binding from the manager and
    /// send [`coordinator::CoordinatorEvent::Cancel`] to the coordinator so
    /// any in-flight [`coordinator::Stage::Recording`] is reset to Idle.
    ///
    /// Call this in `RuntimeSupervisor::stop()` so PTT presses while the
    /// runtime is down do not accumulate stale state. A coordinator stuck in
    /// [`coordinator::Stage::Processing`] (transcription was in-flight when
    /// stop fired) is not fully reset by Cancel — it transitions to Idle on
    /// the next [`coordinator::CoordinatorEvent::ProcessingFinished`]. That is
    /// acceptable because Python stays enabled for actual recording lifecycle
    /// (Fix 1, PR #373) so correctness is unaffected.
    pub fn suspend(&self) {
        let _ = self.manager.unregister();
        self.coordinator.send(CoordinatorEvent::Cancel);
    }

    /// Resume key tracking with the given PTT key names. Call this in
    /// `RuntimeSupervisor::start()` after a prior `suspend()` so the manager
    /// resumes emitting tracker outputs for the (possibly updated) PTT chord.
    ///
    /// Returns `Err(String)` when the manager thread's channel is gone or
    /// the register command fails; callers MUST NOT report Phase-B "installed"
    /// / `state=ready` on that path or the operator sees a green tray while
    /// PTT stays silent (Codex P2 #644 discussion r3659255991). The previous
    /// tracker stays in place on error; PTT will be silent until the next
    /// successful resume.
    pub fn resume(&self, key_names: Vec<String>) -> std::result::Result<(), String> {
        self.manager.register(key_names).map_err(|err| {
            crate::diag::log!("[hotkey] failed to re-register hotkey binding on resume: {err}");
            err
        })
    }

    /// Tear the subsystem down cleanly. Idempotent.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    /// Test-only stub constructor that skips the OS listener entirely
    /// and returns a fully-formed `HotkeyHandle` wired to real
    /// manager + coordinator threads. Lets in-crate tests exercise
    /// downstream code paths (in particular the supervisor's
    /// `attempt_in_process_start` restart path) that operate on a
    /// pre-installed `HotkeyHandle` without needing an X display /
    /// Windows message pump — both of which are absent on the
    /// standard Ubuntu CI runners the `rust-hotkeys` feature test
    /// runs on.
    ///
    /// The stub's manager IS a real manager thread, so
    /// `handle.resume(key_names)` really does register through the
    /// backend-agnostic mpsc — meaning tests can shut the manager
    /// down and observe the `resume` Err path end-to-end. Codex P2
    /// #668 discussion 3664983412 asked for a supervisor-level
    /// regression on the failed-resume path; this seam is what makes
    /// that test possible without a live OS listener.
    ///
    /// The `driver` label is `"test-stub"` so any log line accidentally
    /// carrying it into production output surfaces the test seam
    /// rather than a plausible-looking `"rdev"` / `"evdev"`.
    #[cfg(test)]
    pub(crate) fn install_stub_for_tests(mode: coordinator::Mode) -> Self {
        use crate::hotkey::manager::{driver_common, tracker::KeyTracker};
        use std::sync::atomic::Ordering;
        use std::sync::Mutex as StdMutex;

        let (manager, cmd_rx) = driver_common::manager_channel();
        // Codex P2 #668 discussion 3665741337 changed the
        // manager-channel default to `false`. The stub represents a
        // backend that HAS reached its "installed" moment (that's
        // what makes it a valid drop-in for the resume-fallback
        // supervisor tests), so explicitly stamp `true` here — the
        // shipping backends do the same in their listener-thread
        // bodies. Without this stamp, tests that assert the stub is
        // alive immediately after construction panic and cascade
        // ENV_LOCK poisoning across the whole lib-test binary.
        manager.listener_alive_flag().store(true, Ordering::Relaxed);
        let tracker = Arc::new(StdMutex::new(KeyTracker::new(Vec::new())));
        let manager_thread =
            driver_common::spawn_manager_thread(cmd_rx, tracker).expect("stub manager spawns");
        let options = Options {
            mode,
            auto_complete_processing: true,
        };
        let (coordinator, coordinator_thread) =
            spawn_coordinator(options, |_action| {}, Instant::now);
        let injection_guard = Arc::new(InjectionGuard::new());
        HotkeyHandle {
            coordinator,
            coordinator_thread: Some(coordinator_thread),
            manager,
            manager_thread: Some(manager_thread),
            injection_guard,
            driver: "test-stub",
        }
    }

    /// Test-only accessor to kill just the manager thread of a stub
    /// handle so callers can drive the "manager gone" failure mode of
    /// [`Self::resume`] without also tearing down the coordinator.
    /// Codex P2 #668 discussion 3664983412.
    ///
    /// Also flips the stub's shared `listener_alive` flag to `false`
    /// so a caller polling `is_listener_alive()` sees the dead-
    /// listener state — the shipping backends' listener-thread drop-
    /// guards do this automatically, but the stub has no platform
    /// listener to run one.
    #[cfg(test)]
    pub(crate) fn shutdown_manager_only_for_tests(&mut self) {
        use std::sync::atomic::Ordering;
        self.manager.shutdown();
        if let Some(t) = self.manager_thread.take() {
            t.join();
        }
        self.manager
            .listener_alive_flag()
            .store(false, Ordering::Relaxed);
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
    /// No-op: stub build has no manager to suspend.
    pub fn suspend(&self) {}
    /// No-op: stub build has no manager to resume. Returns Ok so callers
    /// don't need a feature-gate at every use — matches the "install_hotkey
    /// already returned Unsupported before we got here" invariant.
    pub fn resume(&self, _key_names: Vec<String>) -> std::result::Result<(), String> {
        Ok(())
    }
    /// Stub build has no listener installed — the CLI/caller shouldn't be
    /// calling this on a stub handle, but returning a constant lets the
    /// call site type-check without a feature-gate at every use.
    pub fn driver_name(&self) -> &'static str {
        "none"
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
        // `install_hotkey` publishes a fresh `InjectionGuard` into the
        // process-global slot via `inject_guard::set_global` BEFORE it
        // attempts listener startup — so this test mutates a
        // process-global singleton even on the headless path where the
        // install later fails. Hold the crate-wide guard lock so it
        // cannot race `inject_guard_tests::global_slot_last_writer_wins`,
        // whose `Arc::ptr_eq` assertions would otherwise see this
        // test's guard replace theirs mid-assertion. Codex P2 #668
        // discussion 3666165058.
        let _guard_lock = crate::test_env_lock::GLOBAL_GUARD_LOCK
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

    /// Pins the feature-build behaviour of `validate_key_names`, the
    /// pure helper the runtime restart path consults BEFORE parking
    /// Python on a key-binding change (Codex P2 PR #421 runtime.rs:530).
    /// Empty input must surface `EmptyConfig`; an unsupported name must
    /// surface `UnsupportedKey(name)` and stop at the FIRST bad name in
    /// the list (so the message is deterministic). The all-supported
    /// path returns Ok so the restart gate proceeds to `handle.resume`.
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
        // All-supported -> Ok so the restart path proceeds to resume.
        assert!(validate_key_names(&["ctrl_l".to_owned(), "f9".to_owned()]).is_ok());
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

    /// `rust_hotkey_backend_available` reflects the feature gate, not an env
    /// var, so it is constant per binary build. On a stock build it must
    /// always be false; on a `rust-hotkeys` build it must always be true.
    #[test]
    fn backend_available_reflects_feature_gate() {
        // This assertion is always true by the cfg definition:
        assert_eq!(
            rust_hotkey_backend_available(),
            cfg!(feature = "rust-hotkeys"),
            "backend_available must equal the rust-hotkeys feature flag"
        );
    }

    /// When the user sets `VOICEPI_HOTKEY_BACKEND=rust` but the binary was
    /// built without `--features rust-hotkeys`, `backend_available` must be
    /// false (the env var controls `requested`, not `available`).
    #[test]
    #[cfg(not(feature = "rust-hotkeys"))]
    fn backend_available_is_false_on_stock_build_regardless_of_env() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("VOICEPI_HOTKEY_BACKEND").ok();

        std::env::set_var("VOICEPI_HOTKEY_BACKEND", "rust");
        assert!(
            !rust_hotkey_backend_available(),
            "backend_available must be false on a stock build even when env var is set"
        );

        match prev {
            Some(v) => std::env::set_var("VOICEPI_HOTKEY_BACKEND", v),
            None => std::env::remove_var("VOICEPI_HOTKEY_BACKEND"),
        }
    }

    // -----------------------------------------------------------------------
    // HotkeyConfig constructor and stub install_hotkey (non-feature builds).
    // -----------------------------------------------------------------------

    /// `HotkeyConfig::hold_to_talk` is the primary public constructor; verify
    /// that it populates both fields correctly. This test runs on all build
    /// configurations (the constructor is not feature-gated).
    #[test]
    fn hold_to_talk_constructor_sets_key_names_and_mode() {
        let keys = vec!["ctrl_l".to_owned(), "f9".to_owned()];
        let cfg = HotkeyConfig::hold_to_talk(keys.clone());
        assert_eq!(cfg.key_names, keys);
        assert!(
            matches!(cfg.mode, coordinator::Mode::HoldToTalk),
            "hold_to_talk constructor must set Mode::HoldToTalk"
        );
    }

    /// On a stock build (no `rust-hotkeys` feature) `install_hotkey` must
    /// immediately return `InstallError::Unsupported` so the supervisor can
    /// log a one-line warning and keep the pynput path active. No threads
    /// are spawned and no OS resources are acquired.
    #[test]
    #[cfg(not(feature = "rust-hotkeys"))]
    fn install_hotkey_stub_returns_unsupported_error() {
        let cfg = HotkeyConfig::hold_to_talk(vec!["ctrl_l".to_owned()]);
        let err = match install_hotkey(cfg, |_| {}) {
            Ok(_) => panic!("stub install_hotkey must never return Ok"),
            Err(e) => e,
        };
        assert!(
            matches!(err, InstallError::Unsupported),
            "stock build must return InstallError::Unsupported, got: {err}"
        );
    }

    /// On a stock build (no `rust-hotkeys` feature) `validate_key_names` is
    /// a no-op that always returns `Ok(())` -- the supervisor's
    /// `hotkey_handle` is None in stub builds so the restart-path
    /// validation gate (runtime.rs) is dead code, but the function itself
    /// must compile and return Ok for any input so the call site type-
    /// checks under both feature configurations.
    ///
    /// Without this test the 3-line stub body is uncovered in the Sonar
    /// build (which uses `--features ui-egui-glow`, NOT `rust-hotkeys`),
    /// which dragged the new-code coverage gate on PR #421.
    #[test]
    #[cfg(not(feature = "rust-hotkeys"))]
    fn validate_key_names_stub_always_returns_ok() {
        // Empty input: the feature build rejects with EmptyConfig; the
        // stub is unconditionally Ok.
        assert!(validate_key_names(&[]).is_ok());
        // Non-empty input including a name the feature build would reject
        // (`super_l` is not in the rdev key map): stub still returns Ok
        // because the supervisor never consults the result when
        // `hotkey_handle` is None.
        assert!(validate_key_names(&["ctrl_l".to_owned(), "super_l".to_owned()]).is_ok());
    }
}
