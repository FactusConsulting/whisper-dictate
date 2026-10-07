//! The Rust hotkey install funnel.
//!
//! Every entry point — [`install_hotkey`], the copy-last, action-sink,
//! and raw-tap variants, and the diagnostic focus-snapshot form —
//! converges on `install_hotkey_with_context`, which owns push-to-talk
//! acquisition, driver selection, and the manager + coordinator spawns.
//! Split out of `hotkey/mod.rs` so the funnel and its wiring scanners
//! live next to each other.

#[cfg(feature = "rust-hotkeys")]
use std::sync::Arc;
#[cfg(feature = "rust-hotkeys")]
use std::time::Instant;

use super::config::{HotkeyConfig, InstallError, Result};
use super::coordinator::CoordinatorAction;
#[cfg(feature = "rust-hotkeys")]
use super::coordinator::{
    spawn_with_context as spawn_coordinator_with_context, CoordinatorEvent,
    CoordinatorEventContext, Options,
};
use super::handle::HotkeyHandle;
#[cfg(feature = "rust-hotkeys")]
use super::handle::PttOwnershipState;
#[cfg(feature = "rust-hotkeys")]
use super::inject_guard;
#[cfg(feature = "rust-hotkeys")]
use super::inject_guard::InjectionGuard;
#[cfg(feature = "rust-hotkeys")]
use super::manager::{
    self, is_rdev_supported_name, spawn_with_driver as spawn_manager_with_driver, ManagerHandle,
    ManagerThread, RawTap, SpawnError, TrackerOutput,
};
#[cfg(feature = "rust-hotkeys")]
use super::preflight::resolve_driver_kind_for_install;
#[cfg(feature = "rust-hotkeys")]
use super::ptt_lock;

/// Alias for the `(driver_name, ManagerHandle, ManagerThread)` triple that
/// [`manager::spawn_with_raw_tap`] returns. Named so the docs elsewhere in
/// this module can refer to the tuple without inlining its full type.
#[cfg(feature = "rust-hotkeys")]
type SpawnManagerOk = (&'static str, ManagerHandle, ManagerThread);

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
/// so the caller can keep the alternate driver wired if Rust can't take over.
#[cfg(feature = "rust-hotkeys")]
pub fn install_hotkey<F>(config: HotkeyConfig, action_sink: F) -> Result<HotkeyHandle>
where
    F: FnMut(CoordinatorAction) + Send + 'static,
{
    install_hotkey_with_raw_tap(config, action_sink, manager::NoopRawTap)
}

/// One-shot action callbacks keyed by [`TrackerOutput`](tracker::TrackerOutput)
/// variant. Every sink runs on the OS-listener thread, so each must only
/// queue work (never inject, never block).
#[cfg(feature = "rust-hotkeys")]
pub struct HotkeyActionSinks {
    pub copy_last: std::sync::Arc<dyn Fn() + Send + Sync>,
    pub paste_last: std::sync::Arc<dyn Fn() + Send + Sync>,
    pub cycle_mode: std::sync::Arc<dyn Fn() + Send + Sync>,
    pub raw_mode: std::sync::Arc<dyn Fn() + Send + Sync>,
    pub clean_mode: std::sync::Arc<dyn Fn() + Send + Sync>,
    /// Optional gate consulted before a PTT press reaches the
    /// coordinator. The runtime installs one that reports true while a
    /// paste-last burst is in flight, so presses accepted mid-burst
    /// cannot be corrupted by the worker's held-modifier release or the
    /// remaining keystrokes landing under the new recording's modifiers
    /// .
    pub ptt_gate: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
}

#[cfg(feature = "rust-hotkeys")]
impl Default for HotkeyActionSinks {
    fn default() -> Self {
        Self {
            copy_last: std::sync::Arc::new(|| {}),
            paste_last: std::sync::Arc::new(|| {}),
            cycle_mode: std::sync::Arc::new(|| {}),
            raw_mode: std::sync::Arc::new(|| {}),
            clean_mode: std::sync::Arc::new(|| {}),
            ptt_gate: None,
        }
    }
}

/// Pure decision for the tracker bridge: map a tracker output to the
/// coordinator event to forward (or drop it), firing the action sinks
/// for the one-shot actions. Extracted so the PTT gate is unit-testable
/// without a live OS listener install.
#[cfg(feature = "rust-hotkeys")]
pub(super) fn bridge_decision(
    out: TrackerOutput,
    sinks: &HotkeyActionSinks,
) -> Option<CoordinatorEvent> {
    match out {
        TrackerOutput::ChordPress => {
            if sinks.ptt_gate.as_ref().is_some_and(|gate| gate()) {
                None
            } else {
                Some(CoordinatorEvent::Press)
            }
        }
        TrackerOutput::ChordRelease => Some(CoordinatorEvent::Release),
        TrackerOutput::ChordCancel => Some(CoordinatorEvent::Cancel),
        TrackerOutput::CopyLast => {
            (sinks.copy_last)();
            None
        }
        TrackerOutput::PasteLast => {
            (sinks.paste_last)();
            None
        }
        TrackerOutput::CycleMode => {
            (sinks.cycle_mode)();
            None
        }
        TrackerOutput::RawMode => {
            (sinks.raw_mode)();
            None
        }
        TrackerOutput::CleanMode => {
            (sinks.clean_mode)();
            None
        }
    }
}

/// Install PTT with a separate one-shot action callback. The callback runs
/// on the Windows message-loop thread, so it must only queue work.
#[cfg(feature = "rust-hotkeys")]
pub fn install_hotkey_with_copy_last<F, C>(
    config: HotkeyConfig,
    action_sink: F,
    copy_last_sink: C,
) -> Result<HotkeyHandle>
where
    F: FnMut(CoordinatorAction) + Send + 'static,
    C: Fn() + Send + Sync + 'static,
{
    install_hotkey_with_actions(
        config,
        action_sink,
        HotkeyActionSinks {
            copy_last: std::sync::Arc::new(copy_last_sink),
            paste_last: std::sync::Arc::new(|| {}),
            cycle_mode: std::sync::Arc::new(|| {}),
            raw_mode: std::sync::Arc::new(|| {}),
            clean_mode: std::sync::Arc::new(|| {}),
            ptt_gate: None,
        },
    )
}

/// Install PTT with separate one-shot action callbacks for both registered
/// shortcuts. Each sink runs on the Windows message-loop thread, so they
/// must only queue work.
#[cfg(feature = "rust-hotkeys")]
pub fn install_hotkey_with_actions<F>(
    config: HotkeyConfig,
    mut action_sink: F,
    sinks: HotkeyActionSinks,
) -> Result<HotkeyHandle>
where
    F: FnMut(CoordinatorAction) + Send + 'static,
{
    install_hotkey_with_context(
        config,
        move |action, _context| action_sink(action),
        manager::NoopRawTap,
        || CoordinatorEventContext::default(),
        sinks,
    )
}

/// Same as [`install_hotkey`] but also invokes `raw_tap` for every OS key
/// event the rdev listener translates, BEFORE the tracker processes it. The
/// diagnostic `wd hotkey capture` CLI uses this so the operator
/// can see individual keydown/keyup events alongside the chord-level actions.
///
/// The tap runs on the rdev listener thread; keep it cheap and non-blocking.
/// All the validation / startup-failure semantics from [`install_hotkey`]
/// apply unchanged — this function is a thin generalisation.
#[cfg(feature = "rust-hotkeys")]
pub fn install_hotkey_with_raw_tap<F, R>(
    config: HotkeyConfig,
    mut action_sink: F,
    raw_tap: R,
) -> Result<HotkeyHandle>
where
    F: FnMut(CoordinatorAction) + Send + 'static,
    R: RawTap,
{
    install_hotkey_with_context(
        config,
        move |action, _context| action_sink(action),
        raw_tap,
        || CoordinatorEventContext::default(),
        HotkeyActionSinks::default(),
    )
}

/// Diagnostic install variant that snapshots focus in the OS-listener
/// callback and delivers that immutable value with the resulting coordinator
/// action. Production callers keep using [`install_hotkey_with_raw_tap`].
#[cfg(feature = "rust-hotkeys")]
pub(crate) fn install_hotkey_with_focus_snapshot<F, R, S>(
    config: HotkeyConfig,
    mut action_sink: F,
    raw_tap: R,
    focus_snapshot: S,
) -> Result<HotkeyHandle>
where
    F: FnMut(CoordinatorAction, Option<bool>) + Send + 'static,
    R: RawTap,
    S: Fn() -> Option<bool> + Send + Sync + 'static,
{
    install_hotkey_with_context(
        config,
        move |action, context| action_sink(action, context.source_focus),
        raw_tap,
        move || CoordinatorEventContext {
            source_focus: focus_snapshot(),
        },
        HotkeyActionSinks::default(),
    )
}

#[cfg(feature = "rust-hotkeys")]
fn install_hotkey_with_context<F, R, S>(
    config: HotkeyConfig,
    action_sink: F,
    raw_tap: R,
    source_context: S,
    sinks: HotkeyActionSinks,
) -> Result<HotkeyHandle>
where
    F: FnMut(CoordinatorAction, CoordinatorEventContext) + Send + 'static,
    R: RawTap,
    S: Fn() -> CoordinatorEventContext + Send + Sync + 'static,
{
    // Retract any refusal published by an earlier attempt before this one
    // can decide anything. The slot is process-wide and drives the GUI
    // banner, so it must describe the LATEST install attempt -- including
    // the config-error paths just below, which return before the lock is
    // ever consulted and would otherwise leave a stale refusal on screen.
    ptt_lock::report::clear();

    if config.key_names.is_empty() {
        return Err(InstallError::EmptyConfig);
    }
    // Resolve the driver early so the validator can consult the correct
    // supported-name table. RegisterHotKey accepts a few additional Windows
    // virtual keys, while both drivers share the common names used by the
    // default configuration.
    let driver_kind = resolve_driver_kind_for_install(&config.key_names);
    match driver_kind {
        #[cfg(target_os = "windows")]
        crate::hotkey::manager::DriverKind::Register => {
            // Parse via the register driver's own validator. On the fallback
            // path (`resolve_driver_kind_for_install` downgraded to Rdev
            // because parse_chord already failed), we don't reach this arm
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
            // disabled the alternate listener for a binding that can never fire.
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

    // Single-owner guard. This is the ONLY place both hotkey drivers pass
    // through, which is why it has to live here: `rdev` installs a passive
    // low-level hook that never conflicts with anything, and
    // `RegisterHotKey` only reports a clash inside its own process, so
    // neither driver can see the other. On 2026-07-29 that blindness let
    // the tray GUI (RegisterHotKey) and `dictate-run` (rdev) both own F9,
    // and one press had both of them typing into the focused window at
    // once. See [`ptt_lock`] for the full account.
    //
    // Taken BEFORE any thread is spawned so a refused process leaves
    // nothing behind, and released by `HotkeyHandle`'s `Drop` (or by the
    // kernel, if this process dies without unwinding).
    let chord = config.key_names.join("+");
    let ptt_lock = match ptt_lock::acquire_or_refuse(&chord, manager::driver_label(driver_kind)) {
        ptt_lock::PttOwnership::Owned(lock) => PttOwnershipState::Held { _lock: lock },
        ptt_lock::PttOwnership::Unguarded => PttOwnershipState::Inactive,
        ptt_lock::PttOwnership::Refused(conflict) => {
            return Err(InstallError::AlreadyHeld {
                chord,
                holder_pid: conflict.holder_pid(),
                holder_desc: conflict.holder_description(),
            })
        }
    };

    let options = Options {
        mode: config.mode,
        auto_complete_processing: config.auto_complete_processing,
    };
    let (coord_handle, coord_thread) =
        spawn_coordinator_with_context(options, action_sink, Instant::now);

    // Shared self-injection guard — armed by the injector wrapper around
    // every SendInput burst, checked by the driver callback below. See
    // [`inject_guard`] for the full rationale (Windows PTT wedge; the
    // Linux/Wayland sibling has no /dev/input equivalent).
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
            let Some(event) = bridge_decision(out, &sinks) else {
                return;
            };
            bridge.send_with_context(event, source_context());
        },
        raw_tap,
    ) {
        Ok(triple) => triple,
        Err(err) => {
            // Listener (or manager-thread) startup failed. Tear the
            // coordinator down so we don't leak the thread, and surface
            // the error to the supervisor so it can keep the alternate driver wired.
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
        ptt_lock,
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
        // Keep the writer-startup reason verbatim AND label it, so the
        // supervisor's fallback line says "the diagnostic pipeline is
        // dead" rather than looking like an OS-hook failure.
        SpawnError::WriterStartup(msg) => {
            format!("async diagnostic writer failed to start: {msg}")
        }
    }
}

#[cfg(not(feature = "rust-hotkeys"))]
pub(crate) fn install_hotkey_with_focus_snapshot<F, R, S>(
    _config: HotkeyConfig,
    _action_sink: F,
    _raw_tap: R,
    _focus_snapshot: S,
) -> Result<HotkeyHandle>
where
    F: FnMut(CoordinatorAction, Option<bool>) + Send + 'static,
    R: Send + Sync + 'static,
    S: Fn() -> Option<bool> + Send + Sync + 'static,
{
    Err(InstallError::Unsupported)
}

/// Stub `install_hotkey` for builds without the feature. Always returns
/// [`InstallError::Unsupported`] — the supervisor's contract is to log a
/// warning and stay on the pynput path in that case.
#[cfg(not(feature = "rust-hotkeys"))]
pub fn install_hotkey<F>(_config: HotkeyConfig, _action_sink: F) -> Result<HotkeyHandle>
where
    F: FnMut(CoordinatorAction) + Send + 'static,
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
    F: FnMut(CoordinatorAction) + Send + 'static,
    R: Send + Sync + 'static,
{
    Err(InstallError::Unsupported)
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
