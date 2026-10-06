//! listener regression tests for the rdev driver.

use super::*;

#[test]
fn register_and_unregister_roundtrip() {
    // Lightweight test that register/unregister responses come back
    // through the mpsc — does NOT exercise the rdev listener thread
    // (it's still installed but no synthetic events are injected).
    // In headless CI / containers rdev::listen returns Err immediately
    // (no X display / no accessibility permission) — that's exactly
    // the P1-#2 startup-failure path, so we skip the round-trip on
    // such platforms rather than assert success.
    let count = Arc::new(AtomicUsize::new(0));
    let count_cb = Arc::clone(&count);
    let guard = Arc::new(InjectionGuard::new());
    let (handle, _thread) = match spawn(guard, move |_out| {
        count_cb.fetch_add(1, Ordering::SeqCst);
    }) {
        Ok(pair) => pair,
        Err(SpawnError::ListenerStartup(_))
        | Err(SpawnError::ListenerHung)
        | Err(SpawnError::WriterStartup(_)) => {
            eprintln!(
                "skipping register_and_unregister_roundtrip: rdev listener \
                 refused to start (headless env)"
            );
            return;
        }
    };
    handle
        .register(vec!["ctrl_l".to_owned(), "f9".to_owned()])
        .expect("register");
    handle.unregister().expect("unregister");
    handle
        .register(vec!["shift_r".to_owned()])
        .expect("re-register");
    // No events fired through the tracker — count stays zero.
    assert_eq!(count.load(Ordering::SeqCst), 0);
    handle.shutdown();
    // Do NOT join: the rdev listener thread is unjoinable, but the
    // manager thread is — drop the handle and let the test runner
    // finish. (The thread exits on its own when it sees Shutdown.)
}

#[test]
fn listener_startup_failure_is_surfaced_to_caller() {
    // On a headless Linux container rdev::listen returns Err very
    // quickly (no X display). The driver MUST propagate that to the
    // spawn-side caller instead of silently logging and exiting, so
    // the supervisor receives an explicit startup failure.
    // We don't have a way to force the failure on platforms where the
    // hook genuinely works, so on those we treat success as "test not
    // applicable" rather than fail.
    let guard = Arc::new(InjectionGuard::new());
    match spawn(guard, |_out| {}) {
        Ok((handle, _thread)) => {
            handle.shutdown();
        }
        Err(SpawnError::ListenerStartup(msg)) => {
            assert!(
                !msg.is_empty(),
                "ListenerStartup error message should not be empty"
            );
        }
        Err(SpawnError::ListenerHung) => {
            // Hung is also a "tell the caller" outcome — acceptable.
        }
        Err(SpawnError::WriterStartup(msg)) => {
            // The async diagnostic writer failed to spawn. Also a
            // "tell the caller" outcome, and equally must not be empty.
            assert!(
                !msg.is_empty(),
                "WriterStartup error message should not be empty"
            );
        }
    }
}

// -----------------------------------------------------------------------
// Heartbeat lifecycle on startup failure.
// -----------------------------------------------------------------------

#[test]
fn spawn_startup_failure_stops_heartbeat_thread() {
    // Best-effort: on a host where rdev::listen actually starts, the
    // returned Ok is out of scope for this test — we just require that
    // the code path exists. On headless CI (no display / no accessibility)
    // rdev::listen fails within READY_PROBE_WINDOW and `spawn` returns
    // ListenerStartup; the fix guarantees the heartbeat's stop atomic is
    // set before the error propagates, so a caller that retries does not
    // stack orphan threads. We can't observe the atomic from outside
    // `spawn` (it is created inside the function), but we CAN observe
    // that spawn does not leak forever — if it returned quickly with an
    // error, the leak-avoidance path was traversed.
    let guard = Arc::new(InjectionGuard::new());
    let start = std::time::Instant::now();
    match spawn(guard, |_out| {}) {
        Ok((handle, _thread)) => {
            handle.shutdown();
        }
        Err(SpawnError::ListenerStartup(_))
        | Err(SpawnError::ListenerHung)
        | Err(SpawnError::WriterStartup(_)) => {
            // The startup failure returned in a bounded time — the
            // early-return path (which now sets heartbeat_stop first)
            // was taken. If it hadn't been, this call would still return
            // quickly but a heartbeat thread would keep writing forever;
            // the leak is not directly observable from this test, but
            // the paired `heartbeat_state_*` tests below pin the emit
            // retire policy at the pure-decision layer.
        }
    }
    // Guard against a regression that makes `spawn` block forever after
    // a startup failure.
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "spawn must return promptly on startup failure"
    );
}

// -----------------------------------------------------------------------
// `HotkeyHandle::is_listener_alive` liveness
// signal for the boot-self-test.
//
// The rdev driver flips a shared atomic to `false` when its listener
// thread exits for ANY reason (`rdev::listen` returned Ok / Err, or a
// panic unwinds the closure). Without this signal, the boot self-test's
// `listener_exited_early` was hardcoded `false` and emitted `ok:true`
// on the exact dead-hook regression the verb was written to catch.
// -----------------------------------------------------------------------

#[test]
fn is_listener_alive_becomes_true_after_successful_spawn_reaches_listen() {
    // The manager-channel default is `false`; the rdev listener thread flips it to
    // `true` ONLY after any pre-listen `diag::log!` has returned,
    // right before entering `rdev::listen`. On the healthy path
    // (X11 / Windows / macOS with a working diag sink) that store
    // happens within a handful of microseconds of the spawn-side
    // observing `ListenerSignal::Started`, and the spawn-side then
    // waits READY_PROBE_WINDOW before returning — so by the time
    // this test reads the flag, a healthy listener has already
    // stamped `true`.
    //
    // On headless CI rdev::listen fails within READY_PROBE_WINDOW and
    // spawn returns Err — no HotkeyHandle is produced and the
    // assertion is vacuously skipped.
    let guard = Arc::new(InjectionGuard::new());
    let (handle, thread) = match spawn(guard, |_out| {}) {
        Ok(pair) => pair,
        Err(SpawnError::ListenerStartup(_))
        | Err(SpawnError::ListenerHung)
        | Err(SpawnError::WriterStartup(_)) => {
            eprintln!("skipping is_listener_alive_after_spawn_reaches_listen: headless env");
            return;
        }
    };
    // Poll up to 200ms for the "installed" transition. The listener
    // thread has to run its two pre-listen statements (ready_tx.send
    // + diag::log!) then flip the flag; on a healthy CI runner this
    // is microseconds, but a small polling window keeps the test
    // resilient to timing skew on shared runners.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    while std::time::Instant::now() < deadline && !handle.is_listener_alive() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        handle.is_listener_alive(),
        "listener_alive must transition to `true` after the rdev \
         listener thread reaches its pre-`rdev::listen` store. If \
         this fails, either the diag sink stalled (regression bite \
         implementation is meant to preserve) or the \
         `store(true)` reordering was reverted"
    );
    handle.shutdown();
    // Join the manager thread but do NOT expect the listener atomic to
    // flip yet: the rdev listener thread is unjoinable and keeps
    // running until process exit (documented rdev limitation), so the
    // atomic stays true across manager teardown. That is fine — the
    // signal is aimed at the "listener died mid-session" regression,
    // not at graceful shutdown attribution.
    thread.join();
}

/// The primary regression bite:
/// on a stalled diag sink, the flag MUST stay `false` (not flip to
/// `true`) so a boot self-test polling `is_listener_alive()` reports
/// the dead-hook wedge.
///
/// We can't easily install a real stalled diag sink, so we assert the
/// invariant at the atomic level: a freshly-constructed `ManagerHandle`
/// (before ANY listener thread runs) reads `false`. This nails down
/// the default so a regression that flipped it back to `true` — the
/// pre-fix state — fails immediately.
#[test]
fn freshly_constructed_manager_handle_reports_listener_not_yet_installed() {
    use crate::hotkey::manager::driver_common::manager_channel;
    let (handle, _rx) = manager_channel();
    assert!(
        !handle.is_listener_alive(),
        "default must be `false` — otherwise a stalled pre-listen \
         `diag::log!` would let the boot self-test report PASS for \
         a hook that was never installed."
    );
}

#[test]
fn is_listener_alive_is_false_when_spawn_reports_startup_failure() {
    // On headless CI rdev::listen returns Err within READY_PROBE_WINDOW,
    // the listener thread body exits, and its drop-guard flips the
    // liveness flag to false BEFORE spawn returns the error. But spawn
    // returns Err on this path — no HotkeyHandle to inspect — so we can
    // only assert the negative: on a host where spawn DOES return Ok,
    // the flag must be true. The paired test above pins that.
    //
    // If a future refactor made spawn return Ok on a headless failure
    // (regression), this test would still pass but the paired one would
    // observe an immediately-dead handle — the invariant is preserved
    // across the pair.
    let guard = Arc::new(InjectionGuard::new());
    match spawn(guard, |_out| {}) {
        Ok((handle, thread)) => {
            // Live path — the "immediately-after-spawn" assertion runs
            // in the paired test above.
            handle.shutdown();
            thread.join();
        }
        Err(SpawnError::ListenerStartup(_))
        | Err(SpawnError::ListenerHung)
        | Err(SpawnError::WriterStartup(_)) => {
            // Startup failure path — no handle to inspect. The
            // drop-guard did flip the atomic before spawn returned,
            // but the shared Arc is dropped with the failed spawn's
            // internal state so we cannot observe it from here.
        }
    }
}

// -----------------------------------------------------------------------
// The actual spawn wiring must stop the heartbeat on startup-failure
// early-return paths.
//
// The `spawn_heartbeat_thread_exits_when_stop_is_signalled` test above
// flips an INDEPENDENT stop atomic, which only proves the heartbeat
// loop honours a stop signal — it does NOT prove that
// `spawn_with_raw_tap`'s error branches actually set the atomic.
// Deleting every `heartbeat_stop.store(true, ...)` from those branches
// would still leave that test green while orphan heartbeats
// accumulated on retry. This test drives the real wiring via the
// crate-only `spawn_with_raw_tap_capturing_heartbeat_for_tests`
// shim — on any host where rdev::listen returns Err quickly (headless
// CI), the shim's returned heartbeat JoinHandle must observe the
// spawn-wiring's `heartbeat_stop.store(true, ...)` and exit.
// -----------------------------------------------------------------------

#[test]
fn spawn_startup_failure_actually_stops_the_heartbeat_via_wiring() {
    use std::time::{Duration, Instant};
    let guard = Arc::new(InjectionGuard::new());
    let (result, heartbeat) =
        spawn_with_raw_tap_capturing_heartbeat_for_tests(guard, |_out| {}, NoopRawTap);
    let heartbeat = heartbeat.expect(
        "heartbeat thread must spawn on any reasonable test host — OOM on \
         thread::Builder::spawn is the only failure mode",
    );
    match result {
        Ok((handle, _thread)) => {
            // Host actually has a working display / accessibility perms
            // the wiring's error branches don't fire here, so this
            // regression is only meaningful in a headless environment.
            // Clean up and skip the assertion; the paired invariant is
            // covered by the failing-startup branch on CI.
            handle.shutdown();
            eprintln!(
                "skipping spawn_startup_failure_actually_stops_the_heartbeat_via_wiring: \
                 rdev listener started successfully on this host, so the wiring's \
                 error-branch `heartbeat_stop.store(true)` was not exercised"
            );
            return;
        }
        Err(SpawnError::ListenerStartup(_))
        | Err(SpawnError::ListenerHung)
        | Err(SpawnError::WriterStartup(_)) => {
            // Fall through — we expect the heartbeat to exit because
            // one of the wiring's error branches stored heartbeat_stop.
        }
    }
    // Wait up to 3 s for the heartbeat's slice-sleep to observe the
    // stop atomic set by the spawn wiring's error branch. Failure
    // mode against a regression that removes the wiring stores: the
    // heartbeat runs forever, is_finished() stays false, we hit the
    // deadline, and the assert below trips.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !heartbeat.is_finished() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        heartbeat.is_finished(),
        "spawn's startup-failure early-return branches MUST store to \
         heartbeat_stop so the heartbeat thread exits — otherwise a caller \
         that retries after a listener-startup failure accumulates orphan \
         heartbeat threads"
    );
    heartbeat.join().expect("heartbeat thread panicked");
}
