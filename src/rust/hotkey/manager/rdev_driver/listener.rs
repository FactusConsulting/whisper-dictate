//! Manager, listener and heartbeat startup wiring.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use super::super::driver_common::{manager_channel, spawn_manager_thread};
use super::super::tracker::{KeyTracker, TrackerOutput};
use super::callback::CallbackContext;
use super::heartbeat::spawn_heartbeat_thread;
use super::listener_thread::run_native_listener;
use super::readiness::{await_listener_ready, ListenerSignal};
#[cfg(test)]
use super::NoopRawTap;
use super::{ManagerHandle, ManagerThread, RawTap, SpawnError};
use crate::hotkey::inject_guard::InjectionGuard;

/// Spawn the manager thread plus the `rdev` listener thread. Every tracker
/// output produced by a real OS key event is dispatched to `on_output`,
/// which the coordinator hooks up to its press/release/cancel events.
///
/// Returns `Err(SpawnError)` if the rdev listener fails to start within
/// [`super::READY_PROBE_WINDOW`] — for example missing X display on Linux, or
/// missing accessibility permission on macOS. On success the listener thread
/// runs forever (rdev limitation) and is reported as healthy.
///
/// `injection_guard` gates the callback: while the injector wrapper is
/// bursting synthetic events through `SendInput` (Windows) / equivalent
/// APIs on X11+macOS, the guard is armed and the callback drops every
/// event rdev delivers. This closes the Windows self-injection PTT
/// wedge — the same class of bug the Wayland fix in #467 solved via
/// device-level filtering. See [`crate::hotkey::inject_guard`] for the
/// full rationale and timing model.
#[cfg(test)]
pub fn spawn<F>(
    injection_guard: Arc<InjectionGuard>,
    on_output: F,
) -> Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    spawn_with_raw_tap(injection_guard, on_output, NoopRawTap)
}

/// Invoke `raw_tap` for every raw OS key event
/// BEFORE the tracker sees it. The tap runs on the rdev listener thread
/// keep it cheap and non-blocking (long work will delay the tracker and
/// starve the coordinator).
///
/// The raw tap DOES run for self-injected events too (before the
/// [`InjectionGuard`] check), so the diagnostic `hotkey capture` CLI
/// can still surface the injector's own SendInput bursts. Only the
/// tracker (and therefore the coordinator's chord state) is protected
/// from the feedback loop.
pub fn spawn_with_raw_tap<F, R>(
    injection_guard: Arc<InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    // Discard the heartbeat handle — production has never had a use for it.
    // See `spawn_with_raw_tap_capturing_heartbeat_for_tests` for the test-only
    // surface that observes it (, thread PRRT_kwDOSfNjQs6UaDcc).
    spawn_with_raw_tap_inner(injection_guard, on_output, raw_tap).0
}

/// Result of the internal spawn helper: the normal spawn outcome plus the
/// heartbeat thread's `JoinHandle` (or `None` if `thread::Builder::spawn`
/// failed to launch it, essentially OOM only). Kept as a type alias to
/// tame `clippy::type_complexity` — the tuple appears on multiple signatures.
type SpawnWithHeartbeatResult = (
    Result<(ManagerHandle, ManagerThread), SpawnError>,
    Option<thread::JoinHandle<()>>,
);

/// Test-only shim that returns both the normal `spawn_with_raw_tap` result
/// AND the heartbeat thread's `JoinHandle`. Callers can then observe that
/// the actual production wiring in `spawn_with_raw_tap_inner`'s error
/// branches (all four `heartbeat_stop.store(true, ...)` calls) genuinely
/// makes the heartbeat thread exit — a property the earlier
/// `spawn_heartbeat_thread_exits_when_stop_is_signalled` test could not
/// pin because it flipped an INDEPENDENT stop atomic instead of exercising
/// the spawn wiring itself. thread PRRT_kwDOSfNjQs6UaDcc.
///
/// The `Option` in the second slot is `None` iff `thread::Builder::spawn`
/// failed to launch the heartbeat thread (essentially OOM only) — tests
/// that require the handle should `expect(...)` it.
#[cfg(test)]
pub(crate) fn spawn_with_raw_tap_capturing_heartbeat_for_tests<F, R>(
    injection_guard: Arc<InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> SpawnWithHeartbeatResult
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    spawn_with_raw_tap_inner(injection_guard, on_output, raw_tap)
}

/// Shared body of `spawn_with_raw_tap` and its test-only companion. Returns
/// the heartbeat `JoinHandle` alongside the usual spawn result so tests can
/// observe that the spawn error-branch wiring actually stops the heartbeat
/// . Production callers ignore the handle.
fn spawn_with_raw_tap_inner<F, R>(
    injection_guard: Arc<InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> SpawnWithHeartbeatResult
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    let (handle, cmd_rx) = manager_channel();
    let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));
    let on_output = Arc::new(on_output);
    let raw_tap = Arc::new(raw_tap);
    // NOTE: the shared off-callback trace writer is installed by
    // `manager_channel()` above — moved there (from an explicit call
    // here) so evdev and win_registerhotkey get it too.
    // discussion 3666165045.

    // Liveness flag the listener thread flips to `false` on exit.
    // Handed to callers via `ManagerHandle::is_listener_alive`
    // `HotkeyHandle::is_listener_alive` so the boot self-test can
    // distinguish "install returned Ok and the listener stayed up"
    // from "install returned Ok but the listener exited before the
    // hold window closed" — the exact dead-hook regression PR #644
    // was written to catch (discussion r3658983542).
    // Passing the same `Arc` into the listener closure below is the
    // only way to observe rdev's per-thread hook lifetime: the OS
    // installs the hook against the calling thread and revokes it
    // when the thread exits, so the atomic flipping to `false` is a
    // one-to-one signal that the hook is now uninstalled.
    let listener_alive_signal = handle.listener_alive_flag();

    // Per-listener event counters — updated on every raw OS event from
    // inside the LL-hook callback, read by the heartbeat thread. Atomics
    // (rather than `Mutex<u64>`) so the callback stays lock-free on the
    // hot path — matters because on Windows the callback runs from
    // inside the LL-hook thread for every desktop-wide keydown/keyup.
    let events_total = Arc::new(AtomicU64::new(0));
    let events_since_heartbeat = Arc::new(AtomicU64::new(0));
    // Heartbeat stop signal — set on Drop of the listener side would be
    // ideal, but the rdev listener thread cannot be joined, and the
    // production `HotkeyHandle` is a process-lifetime resource (never
    // dropped in shipping code). The heartbeat thread therefore runs
    // until process exit; the atomic exists so the test-only shutdown
    // path can nudge it, and so a future rework can wire it in without
    // reshaping the spawn signature.
    let heartbeat_stop = Arc::new(AtomicBool::new(false));

    // Production ignores the returned `JoinHandle` (the heartbeat runs
    // for process life on shipping installs and the `HotkeyHandle` is
    // never dropped). The handle is threaded up through the outer
    // `(_, Option<JoinHandle>)` return so the test-only companion
    // `spawn_with_raw_tap_capturing_heartbeat_for_tests` can observe the
    // thread actually exits when the spawn error branches store to
    // `heartbeat_stop` — thread PRRT_kwDOSfNjQs6UaDcc.
    // `.ok()` matches the pre-existing "log-and-swallow" behaviour on
    // the essentially-impossible OOM path where the OS refused a thread.
    let heartbeat_handle: Option<thread::JoinHandle<()>> = spawn_heartbeat_thread(
        Arc::clone(&events_total),
        Arc::clone(&events_since_heartbeat),
        Arc::clone(&heartbeat_stop),
    )
    .ok();

    // Listener thread — owns rdev. Translates raw events through the shared
    // tracker. Signals readiness / startup failure on a sync channel so
    // `spawn` can surface a quick-failure to the caller (P1 finding #2).
    let listener_tracker = Arc::clone(&tracker);
    let listener_sink = Arc::clone(&on_output);
    let listener_tap = Arc::clone(&raw_tap);
    let listener_guard = Arc::clone(&injection_guard);
    let listener_total = Arc::clone(&events_total);
    let listener_since = Arc::clone(&events_since_heartbeat);
    let (ready_tx, ready_rx) = mpsc::channel::<ListenerSignal>();
    let listener_alive_for_thread = Arc::clone(&listener_alive_signal);
    let listener_thread = thread::Builder::new()
        .name("vp-hotkey-rdev".to_owned())
        .spawn(move || {
            run_native_listener(
                CallbackContext {
                    listener_tracker,
                    listener_sink,
                    listener_tap,
                    listener_guard,
                    listener_total,
                    listener_since,
                },
                listener_alive_for_thread,
                ready_tx,
            );
        });
    if let Err(e) = listener_thread {
        // the heartbeat thread is already
        // running by the time we get here; if the listener never spawns
        // it would write `listener heartbeat; events_since=0` every 5 s
        // forever, misleadingly signalling a hung LL-hook. Stop the
        // heartbeat before returning so a caller that retries doesn't
        // stack orphans either.
        heartbeat_stop.store(true, Ordering::Relaxed);
        return (
            Err(SpawnError::ListenerStartup(format!(
                "thread spawn failed: {e}"
            ))),
            heartbeat_handle,
        );
    }

    // Wait for the listener thread to report it's up (and, on the
    // WriterFailed arm, that the diagnostic pipeline is dead). Without
    // this we'd race the manager thread's spawn against the OS
    // scheduler. Every failure arm funnels through this ONE early
    // return, which STOPS the heartbeat first — a caller that retries
    // after a startup failure must not accumulate orphan heartbeat
    // threads .
    if let Err(err) = await_listener_ready(&ready_rx) {
        heartbeat_stop.store(true, Ordering::Relaxed);
        return (Err(err), heartbeat_handle);
    }

    let manager_thread = match spawn_manager_thread(cmd_rx, Arc::clone(&tracker)) {
        Ok(t) => t,
        Err(err) => {
            // Same rationale as the listener-thread spawn failure branch
            // above: the heartbeat is already running, so stop it before
            // returning or a retry-happy caller stacks orphans.
            heartbeat_stop.store(true, Ordering::Relaxed);
            return (Err(err), heartbeat_handle);
        }
    };
    (Ok((handle, manager_thread)), heartbeat_handle)
}
