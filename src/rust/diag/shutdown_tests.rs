//! shutdown diagnostic regressions.

use super::diag_test_lock;
use crate::diag::{install_gui_diagnostic_log, reset_level_for_tests, DropLedger};
use crate::diag_shutdown_gate::ShutdownGate;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

// -----------------------------------------------------------------------
// Drain-on-exit for the off-callback async queue.
//
// The writer is a background thread. On process exit it is killed
// without draining, so whatever is still queued is lost - including, on
// a crash-adjacent exit, the very records a support thread needs.
// `drain_and_shutdown` pushes a `Shutdown` sentinel through the SAME
// bounded queue as the records (so everything enqueued before the call
// is necessarily ahead of it), the writer flushes and acks, and the
// caller gives up after a bounded deadline so a wedged sink cannot pin
// process teardown.
// -----------------------------------------------------------------------

/// The drain must flush a backlog that was queued while the sink was
/// parked, and only then acknowledge.
///
/// Un-fixed behaviour (a writer that treats the sentinel as "stop now"
/// instead of "drain, then stop", or no sentinel at all): the ack
/// arrives with records still unwritten, so the sink is short and the
/// tee file loses the tail of the capture.
#[test]
fn drain_and_shutdown_flushes_the_queued_backlog_before_acknowledging() {
    // The pipeline halves under test are parameterised away from the
    // global writer slot and the LEVEL atomic, but a future rework that
    // reached for either would otherwise flake against the rest of the
    // suite.
    let _guard = diag_test_lock();

    const CAPACITY: usize = 64;
    const RECORDS: usize = 32;

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);
    let sink: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let gate: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

    // NOTHING is asserted inside the scope: a panic there unwinds with
    // the writer still parked on the gate, and `thread::scope` would
    // then block forever trying to join it - turning an expected FAILURE
    // into an unkillable HANG. Observe inside, assert outside.
    let (acked, second_elapsed, elapsed) = std::thread::scope(|scope| {
        let (pending_ref, dropped_ref) = (&pending, &dropped);
        let (sink_ref, gate_ref) = (&sink, &gate);
        scope.spawn(move || {
            let mut stalled_once = false;
            crate::diag::run_async_writer_loop(rx, CAPACITY, pending_ref, dropped_ref, |line| {
                if !stalled_once {
                    stalled_once = true;
                    let (lock, cv) = gate_ref;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = cv.wait(released).unwrap();
                    }
                }
                sink_ref.lock().unwrap().push(line.to_owned());
            });
        });

        // Build a real backlog: the writer parks on its first record, so
        // all of these sit in the queue (well inside CAPACITY, so
        // nothing is shed).
        for i in 0..RECORDS {
            crate::diag::enqueue_async_into(
                &tx,
                &admission,
                &pending,
                &dropped,
                format!("record #{i}"),
            );
        }

        // Release the sink only AFTER the drain has started, so the
        // drain genuinely waits on a backlog rather than on an
        // already-idle writer.
        let gate_releaser = &gate;
        scope.spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let (lock, cv) = gate_releaser;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        });

        let started = std::time::Instant::now();
        let acked = crate::diag::drain_and_shutdown_into(
            &tx,
            &admission,
            std::time::Duration::from_secs(10),
        );
        let elapsed = started.elapsed();
        // A second drain against a writer that already stopped must
        // return PROMPTLY rather than sit out the deadline: teardown can
        // run twice on a nested error path. Whether it reports success
        // (`Disconnected` on the way in) or failure (the ack sender died
        // with the receiver) is a race with the writer thread's final
        // drop, and either answer is honest - the stall is the bug.
        let second_started = std::time::Instant::now();
        let _ = crate::diag::drain_and_shutdown_into(
            &tx,
            &admission,
            std::time::Duration::from_secs(10),
        );
        let second_elapsed = second_started.elapsed();
        (acked, second_elapsed, elapsed)
    });

    assert!(
        acked,
        "the writer must acknowledge the drain; false here means the \
         sentinel never reached it and every queued record would be lost \
         at process exit"
    );
    assert!(
        second_elapsed < std::time::Duration::from_secs(5),
        "draining an already-stopped writer must return promptly, not sit \
         out the whole deadline; took {second_elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the drain must return as soon as the writer acks; took {elapsed:?}"
    );

    let recorded = sink.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        RECORDS,
        "every record queued before the drain must reach the sink BEFORE \
         the ack; got {} of {RECORDS}: {recorded:?}",
        recorded.len()
    );
    assert_eq!(
        recorded.first().map(String::as_str),
        Some("record #0"),
        "the backlog must be flushed in order; got {recorded:?}"
    );
    assert_eq!(
        recorded.last().map(String::as_str),
        Some(format!("record #{}", RECORDS - 1)).as_deref(),
        "the LAST record queued before exit is the one a wedge repro \
         needs most; got {recorded:?}"
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "a completed drain must leave nothing pending"
    );
}

/// The drain must be BOUNDED: a writer wedged inside its sink with a
/// full queue cannot park process teardown.
///
/// Un-fixed behaviour (a blocking `tx.send(sentinel)` or an unbounded
/// `ack_rx.recv()`): this call never returns and the process hangs on
/// exit instead of losing a few log lines - strictly worse than the bug
/// the drain was added to fix.
#[test]
fn drain_and_shutdown_gives_up_on_a_wedged_writer_within_the_deadline() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const DEADLINE: std::time::Duration = std::time::Duration::from_millis(80);

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);
    let gate: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

    // Observe inside, assert outside (see the sibling test).
    let (acked, elapsed) = std::thread::scope(|scope| {
        let (pending_ref, dropped_ref, gate_ref) = (&pending, &dropped, &gate);
        scope.spawn(move || {
            let mut stalled_once = false;
            crate::diag::run_async_writer_loop(rx, CAPACITY, pending_ref, dropped_ref, |_line| {
                if !stalled_once {
                    stalled_once = true;
                    let (lock, cv) = gate_ref;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = cv.wait(released).unwrap();
                    }
                }
            });
        });

        // Wedge the writer and fill the queue so the sentinel cannot
        // even be handed over - this is the `try_send` polling path.
        for i in 0..(CAPACITY * 4) {
            crate::diag::enqueue_async_into(
                &tx,
                &admission,
                &pending,
                &dropped,
                format!("wedge #{i}"),
            );
        }

        let started = std::time::Instant::now();
        let acked = crate::diag::drain_and_shutdown_into(&tx, &admission, DEADLINE);
        let elapsed = started.elapsed();

        // Let the writer finish so `thread::scope` can join it.
        {
            let (lock, cv) = &gate;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        drop(tx);
        (acked, elapsed)
    });

    assert!(
        !acked,
        "a wedged writer must report an INCOMPLETE drain so the caller \
         can warn the operator that the tee file may be short"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "the drain must be bounded by its deadline ({DEADLINE:?}); a \
         wedged writer pinned teardown for {elapsed:?}"
    );
}

/// The shutdown sweep must not be extended by traffic queued after the
/// sentinel.
///
/// The real scenario: the Windows `WH_KEYBOARD_LL` / rdev callback
/// thread is unjoinable, so it keeps producing records all through
/// teardown - the documented high-rate mouse trace is the case that
/// motivated the queue in the first place. Everything the drain request
/// covered is ordered ahead of the sentinel and therefore already
/// written by the time the writer sees it; anything the sweep pulls
/// afterwards is younger traffic that no `main` on its way out is
/// waiting for. If the sweep follows that traffic, the caller sits out
/// its whole deadline and reports a FAILED drain - warning the operator
/// that the tee file is short - on a run where nothing was lost at all.
///
/// ## The deterministic seam (no sleeps, no scheduling race)
///
/// The producer IS the sink: every line the writer emits immediately
/// re-enqueues one record on the writer's own thread, so each dequeue
/// is replaced before the next `try_recv` runs and the queue provably
/// never runs dry. That is exactly the "producer keeps up with the
/// sink" steady state, reproduced without a second thread to race
/// against. `CAPACITY` (4) is never approached - the steady-state depth
/// is one record plus the sentinel - so nothing is shed and no marker
/// perturbs the count.
///
/// Un-fixed behaviour (`while let Ok(queued) = rx.try_recv()` with no
/// budget): the sweep never sees an empty queue, the ack never arrives,
/// and this fails on "the drain must be acknowledged".
#[test]
fn drain_and_shutdown_is_not_extended_by_traffic_queued_after_the_sentinel() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const DEADLINE: std::time::Duration = std::time::Duration::from_millis(250);

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);
    let feed = tx.clone();
    let keep_feeding = std::sync::atomic::AtomicBool::new(true);

    // Observe inside, assert outside: a panic in the scope would unwind
    // while the writer is still sweeping, and `thread::scope` would then
    // block forever joining it - an expected FAILURE turned into a HANG.
    let (acked, elapsed) = std::thread::scope(|scope| {
        let (pending_ref, dropped_ref) = (&pending, &dropped);
        let feeding_ref = &keep_feeding;
        scope.spawn(move || {
            crate::diag::run_async_writer_loop(rx, CAPACITY, pending_ref, dropped_ref, |_line| {
                if feeding_ref.load(Ordering::Relaxed) {
                    // One record out, one record in - the callback
                    // thread that will not stop firing during teardown.
                    pending_ref.fetch_add(1, Ordering::Relaxed);
                    let _ = feed.try_send(crate::diag::AsyncRecord::Line {
                        drops_before: 0,
                        message: "post-sentinel callback trace".to_owned(),
                    });
                }
            });
        });

        // Prime the self-sustaining feed; from here the queue is never
        // empty again until `keep_feeding` is cleared.
        crate::diag::enqueue_async_into(&tx, &admission, &pending, &dropped, "seed".to_owned());

        let started = std::time::Instant::now();
        let acked = crate::diag::drain_and_shutdown_into(&tx, &admission, DEADLINE);
        let elapsed = started.elapsed();

        // Let the writer out so `thread::scope` can join it: on the
        // un-fixed tree it is still chasing its own feed.
        keep_feeding.store(false, Ordering::Relaxed);
        (acked, elapsed)
    });

    assert!(
        acked,
        "the drain must be acknowledged even while a producer keeps \
         feeding the queue: every record the request covered was ordered \
         AHEAD of the sentinel and is already written, so following the \
         younger traffic only burns the caller's {DEADLINE:?} budget and \
         reports a lost-records warning on a run that lost nothing"
    );
    assert!(
        elapsed < DEADLINE,
        "an acknowledged drain must return well inside its deadline; a \
         sweep dragged along by post-sentinel traffic took {elapsed:?} of \
         {DEADLINE:?}"
    );
}

/// The post-drain warning path must never wait on the tee-file mutex.
///
/// This is the exact deadlock the deadline exists to avoid: the
/// likeliest reason a drain timed out is that the writer thread is
/// parked INSIDE `write_line_to` holding this mutex, so a blocking
/// `log!` in the timeout branch would queue behind the wedged writer and
/// hang teardown forever.
///
/// Un-fixed behaviour (`write_line`'s blocking `diag_file().lock()`):
/// this test deadlocks on its own held guard instead of returning
/// `false`.
#[test]
fn write_line_nonblocking_skips_the_tee_when_the_mutex_is_contended() {
    let _guard = diag_test_lock();
    // The sink short-circuits at `Off`; make sure a previous test's
    // level choice cannot mask the contract under test.
    reset_level_for_tests();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nonblocking.log");
    install_gui_diagnostic_log(&path).expect("install nonblocking sink");

    // Uncontended: the tee write is attempted and lands.
    assert!(
        crate::diag::write_line_nonblocking("[test] uncontended nonblocking line"),
        "an uncontended tee mutex must still produce the file write - the \
         non-blocking variant is a fallback, not a stderr-only sink"
    );

    let started = std::time::Instant::now();
    let attempted_tee = {
        // Hold the very mutex a wedged writer would be holding.
        let _held = crate::diag::tee_mutex_for_tests().lock().unwrap();
        crate::diag::write_line_nonblocking("[test] contended nonblocking line")
    };
    let elapsed = started.elapsed();

    assert!(
        !attempted_tee,
        "with the tee mutex held, the line must go to stderr only; true \
         here means the write blocked on (or bypassed) the mutex"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "write_line_nonblocking must `try_lock`, never `lock`; the \
         contended call took {elapsed:?}"
    );

    let contents = std::fs::read_to_string(&path).expect("read tee file");
    assert!(
        contents.contains("[test] uncontended nonblocking line"),
        "the uncontended write must be in the tee file; got {contents:?}"
    );
    assert!(
        !contents.contains("[test] contended nonblocking line"),
        "the contended write must have been SKIPPED, not queued behind the \
         mutex; got {contents:?}"
    );
}

/// Unexpected writers must be rejected during shutdown.
/// disconnect is a drain FAILURE, not a success.
///
/// Reachable in production: `ensure_async_writer` deliberately swallows
/// a thread-spawn error, so the sender can be installed with no writer
/// behind it at all; a writer that panicked drops its receiver the same
/// way. Either way the sentinel never reaches anybody, nothing
/// acknowledges the drain, and whatever was queued dies with the
/// process. Reporting `true` there suppresses the caller's exit warning
/// on exactly the runs where diagnostics WERE lost.
///
/// Fully deterministic - no threads, no timing: the receiver is dropped
/// before the call, so `try_send` reports `Disconnected` on its first
/// attempt.
///
/// Un-fixed behaviour (`Err(TrySendError::Disconnected(_)) => true`):
/// panicked "a drain whose writer is GONE must report failure".
#[test]
fn drain_and_shutdown_reports_failure_when_the_writer_is_disconnected() {
    let _guard = diag_test_lock();

    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(4);
    // Whatever is still queued dies here, exactly as it would if the
    // writer thread had panicked or never spawned.
    drop(rx);

    let started = std::time::Instant::now();
    let acked = crate::diag::drain_and_shutdown_into(
        &tx,
        &admission,
        std::time::Duration::from_millis(500),
    );
    let elapsed = started.elapsed();

    assert!(
        !acked,
        "a drain whose writer is GONE must report failure: nothing \
         acknowledged the shutdown and every queued record was lost, so \
         `true` here silently suppresses the exit warning that tells the \
         operator the tee file may be short"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "a disconnected channel must be detected immediately, not waited \
         out for the whole deadline; took {elapsed:?}"
    );
}
