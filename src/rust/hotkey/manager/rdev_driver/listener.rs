//! Manager, listener and heartbeat startup wiring.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use super::super::driver_common::{
    manager_channel_with_listener_alive, spawn_manager_thread_with_exit_hook,
};
use super::super::tracker::{KeyTracker, TrackerOutput};
use super::callback::CallbackContext;
use super::dispatch::{EVENT_SINK, NATIVE_LISTENER};
use super::heartbeat::spawn_heartbeat_thread;
use super::listener_thread::run_native_listener;
use super::readiness::{await_listener_ready, ListenerSignal};
#[cfg(test)]
use super::NoopRawTap;
use super::{ManagerHandle, ManagerThread, RawTap, SpawnError};
use crate::hotkey::inject_guard::InjectionGuard;

/// Spawn the manager thread and route the process-wide `rdev` listener's
/// events to this install. Every tracker output produced by a real OS key
/// event is dispatched to `on_output`, which the coordinator hooks up to its
/// press/release/cancel events.
///
/// The first install starts the listener thread; later installs reuse it
/// and only swap the event sink, because `rdev::listen` cannot be stopped
/// and every extra call would add another OS hook. Shutting the returned
/// manager down clears the sink, leaving the hook in place as a no-op.
///
/// Returns `Err(SpawnError)` if the rdev listener fails to start within
/// [`super::READY_PROBE_WINDOW`] — for example missing X display on Linux, or
/// missing accessibility permission on macOS.
///
/// `injection_guard` gates the callback: while the injector wrapper is
/// bursting synthetic events through `SendInput` (Windows) / equivalent
/// APIs on X11+macOS, the guard is armed and the callback drops every
/// event rdev delivers. This closes the Windows self-injection PTT
/// wedge — the same class of bug Wayland solves via
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
    // surface that observes it.
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
/// branches (every `heartbeat_stop.store(true, ...)` call) genuinely
/// makes the heartbeat thread exit — a property the earlier
/// `spawn_heartbeat_thread_exits_when_stop_is_signalled` test could not
/// pin because it flipped an INDEPENDENT stop atomic instead of exercising
/// the spawn wiring itself.
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
    // Every rdev handle shares the process-wide listener's liveness flag,
    // surfaced through `ManagerHandle::is_listener_alive` /
    // `HotkeyHandle::is_listener_alive` so the boot self-test can
    // distinguish "install returned Ok and the listener stayed up" from
    // "install returned Ok but the listener exited before the hold window
    // closed" — the dead-hook regression class. The OS installs the hook
    // against the listener thread and revokes it when the thread exits, so
    // the flag flipping to `false` is a one-to-one signal that the hook is
    // gone.
    let (handle, cmd_rx) = manager_channel_with_listener_alive(NATIVE_LISTENER.alive_flag());
    let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));

    // Per-install event counters — updated on every raw OS event from
    // inside the LL-hook callback while this install's sink is active, read
    // by the heartbeat thread. Atomics (rather than `Mutex<u64>`) so the
    // callback stays lock-free on the hot path — matters because on Windows
    // the callback runs from inside the LL-hook thread for every
    // desktop-wide keydown/keyup.
    let events_total = Arc::new(AtomicU64::new(0));
    let events_since_heartbeat = Arc::new(AtomicU64::new(0));
    // Heartbeat stop signal: set by every startup-failure branch below and
    // by the manager thread's exit hook when the install is torn down.
    let heartbeat_stop = Arc::new(AtomicBool::new(false));

    // Production ignores the returned `JoinHandle`. The handle is threaded
    // up through the outer `(_, Option<JoinHandle>)` return so the
    // test-only companion `spawn_with_raw_tap_capturing_heartbeat_for_tests`
    // can observe the thread actually exits when the spawn error branches
    // store to `heartbeat_stop`.
    // `.ok()` matches the "log-and-swallow" behaviour on the
    // essentially-impossible OOM path where the OS refused a thread.
    let heartbeat_handle: Option<thread::JoinHandle<()>> = spawn_heartbeat_thread(
        Arc::clone(&events_total),
        Arc::clone(&events_since_heartbeat),
        Arc::clone(&heartbeat_stop),
    )
    .ok();

    // Every failure arm funnels through this ONE early return, which STOPS
    // the heartbeat first — a caller that retries after a startup failure
    // must not accumulate orphan heartbeat threads.
    if let Err(err) = NATIVE_LISTENER.ensure_started(start_native_listener) {
        heartbeat_stop.store(true, Ordering::Relaxed);
        return (Err(err), heartbeat_handle);
    }

    let callback = CallbackContext {
        listener_tracker: Arc::clone(&tracker),
        listener_sink: Arc::new(on_output),
        listener_tap: Arc::new(raw_tap),
        listener_guard: injection_guard,
        listener_total: events_total,
        listener_since: events_since_heartbeat,
    };
    let sink_token = EVENT_SINK.install(Arc::new(move |event: rdev::Event| {
        callback.handle_event(event)
    }));

    let teardown_heartbeat_stop = Arc::clone(&heartbeat_stop);
    let manager_thread = match spawn_manager_thread_with_exit_hook(cmd_rx, tracker, move || {
        EVENT_SINK.clear(sink_token);
        teardown_heartbeat_stop.store(true, Ordering::Relaxed);
    }) {
        Ok(t) => t,
        Err(err) => {
            EVENT_SINK.clear(sink_token);
            heartbeat_stop.store(true, Ordering::Relaxed);
            return (Err(err), heartbeat_handle);
        }
    };
    (Ok((handle, manager_thread)), heartbeat_handle)
}

/// Start the process-wide listener thread and run the readiness handshake.
/// Handed to [`super::dispatch::NativeListener::ensure_started`], which only
/// calls it while no listener thread is running.
///
/// The thread handle is kept whenever the thread may still reach
/// `rdev::listen` (healthy or hung startup) so the next install reuses it
/// rather than stacking a second hook. On a reported startup failure the
/// thread is already exiting, so it is joined and the next install starts
/// a fresh one.
fn start_native_listener(
    listener_alive: Arc<AtomicBool>,
) -> (Option<thread::JoinHandle<()>>, Result<(), SpawnError>) {
    let (ready_tx, ready_rx) = mpsc::channel::<ListenerSignal>();
    let spawned = thread::Builder::new()
        .name("vp-hotkey-rdev".to_owned())
        .spawn(move || run_native_listener(listener_alive, ready_tx));
    let join = match spawned {
        Ok(join) => join,
        Err(e) => {
            return (
                None,
                Err(SpawnError::ListenerStartup(format!(
                    "thread spawn failed: {e}"
                ))),
            );
        }
    };
    match await_listener_ready(&ready_rx) {
        Ok(()) => (Some(join), Ok(())),
        Err(err @ SpawnError::ListenerHung) => (Some(join), Err(err)),
        Err(err) => {
            let _ = join.join();
            (None, Err(err))
        }
    }
}
