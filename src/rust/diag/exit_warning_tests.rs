//! exit warning diagnostic regressions.

use super::{diag_test_lock, release_gate, BlockedTee};
use crate::diag::{install_gui_diagnostic_log, reset_level_for_tests};
use std::sync::{Condvar, Mutex};

/// Production lock ordering must remain consistent,
/// not just the tee mutex held from the test thread.
///
/// `write_line_nonblocking_skips_the_tee_when_the_mutex_is_contended`
/// above holds `diag_file()` directly, so it proves the `try_lock` but
/// says nothing about the OTHER lock production takes. The real wedge
/// has two locks in it: the async writer thread is inside
/// `write_line_to`, and `write_line` handed it the process
/// `std::io::stderr()` guard. If `write_line_to` keeps that guard across
/// its blocking `diag_file().lock()`, a wedged AppData volume pins the
/// process stderr lock as well - and then `write_line_nonblocking`
/// blocks on `stderr.lock()` and NEVER REACHES its `try_lock`. The
/// 500 ms `DIAG_DRAIN_DEADLINE` buys nothing: teardown hangs on the
/// stderr lock instead of the tee mutex, which is exactly the hang the
/// non-blocking sink exists to prevent.
///
/// So this reproduces the production ordering: a thread takes the real
/// stderr guard and hands it to `write_line_to` (the same call
/// `write_line` makes) while this thread holds the tee mutex, then a
/// second thread runs the teardown-warning path.
///
/// Deterministic seam, no sleeps: the wedger announces itself only
/// AFTER it holds the stderr lock, so by the time the probe starts the
/// contended state provably exists. Every rendezvous receive is bounded,
/// so a harness bug asserts instead of hanging.
///
/// Nothing panics while `held` is alive: unwinding through a live
/// `MutexGuard` would POISON the process-wide tee mutex and silently
/// disable the file write for every later test in this binary.
///
/// Un-fixed behaviour (`write_line_to(&mut stderr.lock(), ...)`, guard
/// alive across the tee lock): the probe never returns and this test
/// fails on the bounded receive.
#[test]
fn a_wedged_tee_write_does_not_pin_the_stderr_lock_against_the_teardown_warning() {
    let _guard = diag_test_lock();
    reset_level_for_tests();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("lock-ordering.log");
    install_gui_diagnostic_log(&path).expect("install lock-ordering sink");

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let (probe_tx, probe_rx) = std::sync::mpsc::channel::<bool>();

    // Hold the lock a wedged AppData volume would be holding.
    let held = crate::diag::tee_mutex_for_tests().lock().unwrap();

    // The "async writer thread": production ordering, verbatim.
    std::thread::spawn(move || {
        let stderr_guard = std::io::stderr().lock();
        // The token is the seam: it is sent while the stderr lock is
        // provably held, so the probe below cannot start early.
        let _ = ready_tx.send(());
        crate::diag::write_line_to(stderr_guard, "t=0ms [test] wedged writer line");
    });

    let wedger_ready = ready_rx.recv_timeout(std::time::Duration::from_secs(10));

    // The teardown warning path, on its own thread so a regression
    // blocks IT rather than the test body.
    std::thread::spawn(move || {
        let attempted =
            crate::diag::write_line_nonblocking("[test] teardown warning past the wedge");
        let _ = probe_tx.send(attempted);
    });

    let probed = probe_rx.recv_timeout(std::time::Duration::from_secs(5));

    // Release the wedge BEFORE any assertion can unwind.
    drop(held);

    assert!(
        wedger_ready.is_ok(),
        "harness: the wedging thread never reported holding the stderr \
         lock, so nothing about the lock ordering was exercised"
    );
    assert_eq!(
        probed,
        Ok(false),
        "the teardown warning must reach its `try_lock` and report \
         `false` (stderr only) while the tee mutex is wedged. An Err \
         here means it never returned at all - it blocked acquiring the \
         process stderr lock that `write_line_to` was still holding \
         across its blocking tee write, so a wedged AppData sink pins \
         process exit past DIAG_DRAIN_DEADLINE. Release the stderr \
         guard before the tee write."
    );

    // The wedger must still complete its tee write once unblocked
    // releasing stderr early must not have cost the file record.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut contents = String::new();
    while std::time::Instant::now() < deadline {
        contents = std::fs::read_to_string(&path).unwrap_or_default();
        if contents.contains("[test] wedged writer line") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        contents.contains("[test] wedged writer line"),
        "the wedged writer must still land its tee record once the mutex \
         is free; releasing the stderr guard early must not drop the \
         file write. Tee contents: {contents:?}"
    );
    assert!(
        !contents.contains("[test] teardown warning past the wedge"),
        "the teardown warning must have SKIPPED the tee, not queued \
         behind the wedge. Tee contents: {contents:?}"
    );
}

/// With the tee mutex free, the exit-timeout warning must not reach the tee
/// file.
///
/// The predecessor sink (`write_line_nonblocking`) only `try_lock`s. A
/// free mutex - a writer thread that disconnected, or one that released
/// the mutex a microsecond before teardown ran - therefore hands it a
/// SUCCESSFUL lock and it goes on to do a synchronous `writeln!` +
/// `flush` on the same volume that just failed to drain.
///
/// Fully deterministic: no threads, no timing. The tee file's contents
/// are the observation, and the control line proves the tee was live and
/// uncontended for the duration.
///
/// The warning must be suppressed when the tee is unavailable during exit.
#[test]
fn the_exit_timeout_warning_never_reaches_a_free_tee() {
    let _guard = diag_test_lock();
    // The sink short-circuits at `Off`; a previous test's level choice
    // must not be able to mask the contract under test.
    reset_level_for_tests();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("timeout-warning.log");
    install_gui_diagnostic_log(&path).expect("install timeout-warning sink");

    // Control: the tee is installed, live, and its mutex is free.
    crate::diag::log!("[test] tee is live and uncontended");

    let completed = crate::entrypoint::drain_diagnostics_on_exit_with(
        |_deadline| false,
        crate::entrypoint::exit_timeout_warning_sink,
        crate::entrypoint::DIAG_DRAIN_DEADLINE,
    );

    let contents = std::fs::read_to_string(&path).expect("read tee file");
    // Put the process-wide slot back before anything can unwind.
    crate::diag::install_tee_sink_for_tests(None);

    assert!(
        !completed,
        "harness: the injected drain must report a timeout"
    );
    assert!(
        contents.contains("[test] tee is live and uncontended"),
        "harness: the tee was not actually live, so the absence of the \
         warning below proves nothing. Tee contents: {contents:?}"
    );
    assert!(
        !contents.contains(crate::entrypoint::DIAG_DRAIN_TIMEOUT_WARNING),
        "the exit-teardown timeout warning must not have reached the tee \
         file. A `try_lock` succeeds whenever the mutex is free, and the \
         file write behind it is unbounded on the very volume that just \
         failed to drain - so process exit hangs inside the warning about \
         the wedged sink, past DIAG_DRAIN_DEADLINE. Tee contents: \
         {contents:?}"
    );
}

/// A free mutex with a blocked write must also leave exit-timeout handling
/// bounded.
///
/// `write_line_nonblocking_skips_the_tee_when_the_mutex_is_contended`
/// HOLDS the mutex, so it exercises only the `try_lock` miss. Here the
/// mutex is never held by anyone: the warning acquires it on the first
/// attempt and then parks forever inside the sink's `write`. `try_lock`
/// bounds lock acquisition, not the file I/O behind it.
///
/// Every wait is bounded, so a harness bug asserts instead of hanging,
/// and the gate is released (and the process-wide slot restored) BEFORE
/// any assertion can unwind - a live `MutexGuard` unwound through would
/// poison the tee mutex for every later test in this binary.
///
/// The warning path must return even when the tee write is stalled.
#[test]
fn the_exit_timeout_warning_does_not_write_to_a_free_but_blocked_tee() {
    let _guard = diag_test_lock();
    reset_level_for_tests();

    let gate = std::sync::Arc::new((Mutex::new(false), Condvar::new()));
    crate::diag::install_tee_sink_for_tests(Some(Box::new(BlockedTee {
        gate: std::sync::Arc::clone(&gate),
    })));

    let (done_tx, done_rx) = std::sync::mpsc::channel::<bool>();
    let warner = std::thread::spawn(move || {
        // The production wiring, verbatim, with only the drain result
        // forced: a real `drain_and_shutdown` here would stop the
        // process-wide writer thread for every later test.
        let completed = crate::entrypoint::drain_diagnostics_on_exit_with(
            |_deadline| false,
            crate::entrypoint::exit_timeout_warning_sink,
            crate::entrypoint::DIAG_DRAIN_DEADLINE,
        );
        let _ = done_tx.send(completed);
    });

    let returned = done_rx.recv_timeout(std::time::Duration::from_secs(5));

    // Unwedge, reap, restore - all before any assertion can unwind.
    release_gate(&gate);
    let joined = warner.join();
    crate::diag::install_tee_sink_for_tests(None);

    assert_eq!(
        returned,
        Ok(false),
        "the exit-teardown timeout warning must return while the tee sink \
         is stalled. An Err here means it never returned at all: the tee \
         mutex was FREE, so a `try_lock` succeeded and the warning then \
         blocked inside the sink's `write` - pinning process exit on the \
         wedged AppData volume it was trying to warn about, well past \
         DIAG_DRAIN_DEADLINE. Emit the warning through a sink that does \
         not touch the tee."
    );
    assert!(joined.is_ok(), "the warning thread must not have panicked");
}

/// Exit teardown reports a timeout warning when draining exceeds its budget.
/// must not block on the PROCESS STDERR LOCK either.
///
/// This is the third instance of one shape (see
/// `crate::entrypoint::exit_timeout_warning_sink` for all three). When
/// CLI stderr is redirected to a full or stalled pipe, the async writer
/// blocks inside its `writeln!` while HOLDING `std::io::Stderr`'s lock.
/// The warning then wants the same lock, and `std::io::Stderr` has no
/// non-blocking variant to reach for - so choosing a different SINK, as
/// the previous two rounds did, cannot close this. The fix bounds the
/// WAIT instead: the write goes to a detached thread and the exiting
/// thread walks away after `DIAG_EXIT_WARNING_BUDGET`.
///
/// The wedge here is the real `std::io::stderr()` lock, taken by a thread
/// that then parks, which is exactly the state a writer blocked on a
/// stalled pipe leaves it in. The gate is released before any assertion
/// can unwind, and the probe runs on its own thread with a bounded
/// receive so a regression FAILS instead of hanging the suite.
///
/// Un-fixed behaviour (`exit_timeout_warning_sink` calling
/// `crate::diag::write_line_stderr_only` inline): the probe thread blocks
/// on `stderr.lock()` and never returns, and this fails on "must return
/// while a wedged writer holds the stderr lock".
#[test]
fn the_exit_timeout_warning_survives_a_writer_holding_the_stderr_lock() {
    let _guard = diag_test_lock();
    reset_level_for_tests();

    let gate = std::sync::Arc::new((Mutex::new(false), Condvar::new()));
    let (holding_tx, holding_rx) = std::sync::mpsc::channel::<()>();

    // The "async writer thread", blocked mid-write with the process
    // stderr lock in hand.
    let wedger = std::thread::spawn({
        let gate = std::sync::Arc::clone(&gate);
        move || {
            let _stderr_guard = std::io::stderr().lock();
            // Announced only once the lock is provably held, so the probe
            // below cannot start early.
            let _ = holding_tx.send(());
            let (lock, cv) = &*gate;
            let mut released = lock.lock().unwrap_or_else(|p| p.into_inner());
            while !*released {
                released = cv.wait(released).unwrap_or_else(|p| p.into_inner());
            }
        }
    });
    let wedger_ready = holding_rx.recv_timeout(std::time::Duration::from_secs(10));

    let (done_tx, done_rx) = std::sync::mpsc::channel::<bool>();
    let prober = std::thread::spawn(move || {
        // Production wiring, verbatim, with only the drain result forced:
        // a real `drain_and_shutdown` here would stop the process-wide
        // writer thread for every later test.
        let completed = crate::entrypoint::drain_diagnostics_on_exit_with(
            |_deadline| false,
            crate::entrypoint::exit_timeout_warning_sink,
            crate::entrypoint::DIAG_DRAIN_DEADLINE,
        );
        let _ = done_tx.send(completed);
    });
    let returned = done_rx.recv_timeout(std::time::Duration::from_secs(5));

    // Unwedge and reap BEFORE anything can unwind: a live stderr guard
    // unwound through would deadlock every later test that prints.
    release_gate(&gate);
    let wedger_joined = wedger.join();
    let prober_joined = prober.join();

    assert!(
        wedger_ready.is_ok(),
        "harness: the wedging thread never reported holding the stderr \
         lock, so nothing about the contended state was exercised"
    );
    assert_eq!(
        returned,
        Ok(false),
        "the exit-teardown timeout warning must return while a wedged \
         writer holds the process stderr lock. An Err here means it never \
         returned at all: it blocked on `std::io::stderr().lock()`, which \
         has no non-blocking form, so process exit is pinned for as long \
         as the redirected pipe stays stalled - past DIAG_DRAIN_DEADLINE, \
         inside the warning that the deadline expired. Bound the WAIT \
         (emit off-thread), not just the choice of sink."
    );
    assert!(
        wedger_joined.is_ok() && prober_joined.is_ok(),
        "neither the wedging thread nor the warning probe may panic"
    );
}

/// The budget is a bound on the WAIT, not on the write: an `emit` that
/// never returns must cost the caller the budget and no more, and it must
/// not be joined afterwards (a join would hand the pin straight back).
///
/// Un-fixed behaviour (an inline call, or a `join()` on the emitter):
/// this never returns and fails on the bounded receive.
#[test]
fn a_never_returning_warning_costs_the_exiting_thread_only_its_budget() {
    let gate = std::sync::Arc::new((Mutex::new(false), Condvar::new()));
    const BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

    let (done_tx, done_rx) = std::sync::mpsc::channel::<bool>();
    let caller = std::thread::spawn({
        let gate = std::sync::Arc::clone(&gate);
        move || {
            let emitted = crate::entrypoint::emit_warning_off_thread(
                move || {
                    let (lock, cv) = &*gate;
                    let mut released = lock.lock().unwrap_or_else(|p| p.into_inner());
                    while !*released {
                        released = cv.wait(released).unwrap_or_else(|p| p.into_inner());
                    }
                },
                BUDGET,
            );
            let _ = done_tx.send(emitted);
        }
    });

    let returned = done_rx.recv_timeout(std::time::Duration::from_secs(5));
    release_gate(&gate);
    let joined = caller.join();

    assert_eq!(
        returned,
        Ok(false),
        "a warning that never completes must be reported as not emitted \
         and must not detain the caller. An Err here means the caller was \
         still waiting after 5s of a {BUDGET:?} budget - the emitter was \
         joined, or run inline, either of which pins process exit on \
         whatever the sink is blocked on"
    );
    assert!(joined.is_ok(), "the calling thread must not have panicked");
}

/// The other half: a healthy sink must still produce the warning, and
/// promptly. A bound that always gives up would satisfy the test above
/// while silently deleting the diagnostic.
#[test]
fn a_healthy_warning_is_still_emitted_within_the_budget() {
    let seen = std::sync::Arc::new(Mutex::new(false));
    let emitted = crate::entrypoint::emit_warning_off_thread(
        {
            let seen = std::sync::Arc::clone(&seen);
            move || *seen.lock().unwrap_or_else(|p| p.into_inner()) = true
        },
        crate::entrypoint::DIAG_EXIT_WARNING_BUDGET,
    );
    assert!(
        emitted,
        "a warning whose sink returns immediately must be reported as \
         emitted; `false` here means the bound is swallowing the \
         diagnostic on healthy runs too"
    );
    assert!(
        *seen.lock().unwrap_or_else(|p| p.into_inner()),
        "the emitter closure must actually have run - the off-thread hop \
         must not turn the warning into a no-op"
    );
}
