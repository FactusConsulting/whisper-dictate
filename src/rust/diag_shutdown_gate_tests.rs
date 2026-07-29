//! Companion tests for [`crate::diag_shutdown_gate`] and the drain path
//! it guards.
//!
//! Kept out of `diag_tests.rs` (already 3k lines) and out of an inline
//! `#[cfg(test)] mod tests` so the regression-test discipline scanner
//! (per AGENTS.md) sees a companion file next to the production module.

#![cfg(test)]

use crate::diag::{
    drain_and_shutdown_into, drain_and_shutdown_into_after, enqueue_async_into,
    run_async_writer_loop, AsyncRecord, DropLedger,
};
use crate::diag_shutdown_gate::ShutdownGate;
use crate::diag_test_lock::DIAG_WRITER_LOCK;
use crate::diag_tests::scan_fn_body;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

/// Serialise against the rest of the diag suite. These tests do not touch
/// the process-wide writer slot or the level atomic, but one of them runs
/// a hot spinning producer for as long as the drain takes, and letting
/// that overlap a timing assertion in a sibling test is how flakes are
/// born.
fn diag_test_lock() -> std::sync::MutexGuard<'static, ()> {
    DIAG_WRITER_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

// -----------------------------------------------------------------------
// The latch itself.
// -----------------------------------------------------------------------

#[test]
fn a_fresh_gate_admits_and_a_closed_one_does_not() {
    let gate = ShutdownGate::new();
    assert!(
        gate.admits() && !gate.is_closed(),
        "a fresh gate must admit: the queue accepts trace lines for the \
         whole life of the process, and only teardown closes it"
    );
    gate.close();
    assert!(
        !gate.admits() && gate.is_closed(),
        "a closed gate must refuse: this is the whole mechanism that stops \
         a producer from taking back the slot the shutdown sentinel needs"
    );
}

#[test]
fn closing_the_gate_twice_is_a_no_op() {
    // Teardown can run twice on a nested error path (the `TeardownGuard`
    // in `entrypoint` plus an explicit drain), so a second close must be
    // an ordinary no-op rather than anything observable.
    let gate = ShutdownGate::new();
    gate.close();
    gate.close();
    assert!(
        !gate.admits(),
        "a second close must leave the gate closed, not toggle it back open"
    );
}

// -----------------------------------------------------------------------
// The producer side: what a closed gate does to `enqueue_async_into`.
// -----------------------------------------------------------------------

/// A refused line must cost the channel nothing AND must not disturb a
/// gap the ledger is still holding.
///
/// The second half is the subtle one: the gate check runs BEFORE
/// `DropLedger::take_unbound`, so a genuine pre-teardown overload stays on
/// the ledger and is named by the writer's shutdown close exactly as it
/// was before the gate existed. A gate that checked after the reservation
/// would swallow the count into a line it then threw away, and the gap
/// that happened before teardown would never be reported.
///
/// Un-fixed behaviour (no gate at all): the record is accepted, so
/// `try_recv` yields it and the ledger's unbound counter has been emptied.
#[test]
fn a_closed_gate_refuses_new_lines_without_disturbing_the_ledger() {
    const CAPACITY: usize = 4;
    const GAP: u64 = 3;

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let gate = ShutdownGate::new();
    let (tx, rx) = sync_channel::<AsyncRecord>(CAPACITY);

    // A gap that happened BEFORE teardown and still has to be reported.
    dropped.shed_for_tests(GAP);
    gate.close();

    enqueue_async_into(
        &tx,
        &gate,
        &pending,
        &dropped,
        "raw= callback still firing".to_owned(),
    );

    assert!(
        rx.try_recv().is_err(),
        "a line offered to a closed gate must never reach the channel: \
         every slot it takes is a slot the shutdown sentinel is polling for"
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "a refused line must not be counted as pending; the writer will \
         never see it, so a non-zero count here would hang \
         `flush_async_for_tests` forever"
    );
    assert_eq!(
        (dropped.unbound(), dropped.unnamed()),
        (GAP, GAP),
        "a refused line must leave the ledger untouched. The gate check \
         belongs BEFORE `take_unbound`: a gap that happened before \
         teardown is part of what the drain must report, and folding it \
         into a record that is then discarded loses it outright"
    );
}

// -----------------------------------------------------------------------
// The starvation regression (Codex P2 #681 comment 3669689764).
// -----------------------------------------------------------------------

/// The deterministic half: a producer that refills every slot the writer
/// frees must not be able to keep the shutdown sentinel out of the queue.
///
/// ## The scenario
///
/// The bounded queue is saturated by the documented high-rate mouse trace
/// against a slow-but-perfectly-functional sink. The teardown thread naps
/// `ASYNC_DRAIN_POLL_INTERVAL` between `try_send` attempts, while the rdev
/// / raw-hook callback thread is unjoinable and offers a record roughly
/// every millisecond. Every slot the writer frees is therefore handed
/// straight back to the producer before teardown wakes up to retry, so the
/// sentinel starves for the whole deadline and the process exits with the
/// pre-request backlog undrained - having warned the operator about a
/// writer that was never wedged.
///
/// ## The deterministic seam (no threads, no sleeps, no timing)
///
/// [`drain_and_shutdown_into_after`] runs its closure immediately before
/// each `try_send`, which is exactly the window Codex names. The closure
/// plays both other actors on this one thread: the writer dequeues one
/// record (freeing a slot), then the producer offers the next line into
/// it. The interleaving is chosen by construction rather than raced for,
/// so the outcome is identical on every run.
///
/// The drain reports `false` either way here - there is no writer thread,
/// so nothing can acknowledge - which is why the assertion is on whether
/// the sentinel was ACCEPTED. Acceptance is the property the gate exists
/// to restore; the end-to-end ack is the sibling test below.
///
/// Un-fixed behaviour (polling with no gate): the producer takes every
/// freed slot, `try_send` returns `Full` until the deadline expires, and
/// this fails on "the shutdown sentinel must reach the queue".
#[test]
fn the_shutdown_sentinel_is_not_starved_by_a_producer_refilling_every_freed_slot() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const DEADLINE: Duration = Duration::from_millis(60);

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let gate = ShutdownGate::new();
    let (tx, rx) = sync_channel::<AsyncRecord>(CAPACITY);

    // Saturated before teardown starts, exactly as the mouse trace leaves
    // it against a sink that cannot keep up.
    for i in 0..CAPACITY {
        enqueue_async_into(&tx, &gate, &pending, &dropped, format!("raw= move #{i}"));
    }

    let mut freed = 0usize;
    let acked = drain_and_shutdown_into_after(&tx, &gate, DEADLINE, || {
        // The writer makes one record's worth of progress...
        if rx.try_recv().is_ok() {
            freed += 1;
            pending.fetch_sub(1, Ordering::Relaxed);
        }
        // ...and the callback thread that will not stop firing offers the
        // next line into the slot that just came free.
        enqueue_async_into(
            &tx,
            &gate,
            &pending,
            &dropped,
            "raw= move (still firing)".to_owned(),
        );
    });

    let mut sentinel_accepted = false;
    while let Ok(record) = rx.try_recv() {
        if matches!(record, AsyncRecord::Shutdown(_)) {
            sentinel_accepted = true;
        }
    }

    assert!(
        freed > 0,
        "harness: the writer must have made progress, otherwise this is \
         the wedged-writer case the deadline already covers rather than \
         the starvation case under test"
    );
    assert!(
        sentinel_accepted,
        "the shutdown sentinel must reach the queue. The writer was \
         freeing a slot before every single attempt, so a `Full` for the \
         whole {DEADLINE:?} means the still-firing producer took each one \
         first - the backlog the caller asked to flush is then lost at \
         process exit, and the operator is warned about a writer that was \
         never wedged. Stop admitting new lines before polling for \
         sentinel space"
    );
    assert!(
        !acked,
        "harness: there is no writer thread in this test, so nothing can \
         acknowledge the drain; acceptance of the sentinel is the property \
         under test here and the ack is the sibling test's job"
    );
}

/// The end-to-end half: a real writer thread, a real producer thread that
/// never stops, and a drain that must still be ACKNOWLEDGED inside its
/// deadline.
///
/// The sink is slow but entirely functional (it sleeps per line), which is
/// the case the bug hides in: nothing is wedged, so every existing
/// bounded-drain test stays green while the sentinel is starved anyway.
/// The producer spins with no pacing at all, so it wins every race for a
/// freed slot that it is allowed to enter - which is precisely the point,
/// because once the gate is closed it is not allowed to enter any.
///
/// The drain is not started until the producer has PROVABLY saturated the
/// queue. A shed can only happen against a full channel, so a non-zero
/// ledger is the steady state itself rather than a guess at one - without
/// that wait the drain's very first `try_send` lands before the producer
/// has filled anything and the test proves nothing.
///
/// The scope is joinable on BOTH trees: the cleanup sentinel after the
/// producer stops guarantees the writer loop returns even on the un-fixed
/// tree, where it never received ours. Without it a regression would HANG
/// the suite instead of failing it.
///
/// Un-fixed behaviour (polling with no gate): the producer refills every
/// slot the writer frees, the sentinel never fits, and this fails on "the
/// drain must be acknowledged".
#[test]
fn a_continuously_saturated_producer_cannot_starve_the_shutdown_sentinel() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    // Slow, but the writer is genuinely progressing - one record per nap.
    const SINK_COST: Duration = Duration::from_millis(10);
    const DEADLINE: Duration = Duration::from_millis(500);
    /// Sheds that must be on the ledger before teardown starts. Any
    /// non-zero count proves the queue was full at least once; a small run
    /// proves it is STAYING full.
    const SATURATION_EVIDENCE: u64 = 32;

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let gate = ShutdownGate::new();
    let (tx, rx) = sync_channel::<AsyncRecord>(CAPACITY);
    let keep_producing = AtomicBool::new(true);

    // Observe inside, assert outside: a panic inside the scope would
    // unwind with the writer still sleeping and `thread::scope` would
    // block joining it, turning an expected FAILURE into a HANG.
    let (acked, elapsed, saturated) = std::thread::scope(|scope| {
        let (pending_ref, dropped_ref, gate_ref) = (&pending, &dropped, &gate);
        let producing_ref = &keep_producing;

        scope.spawn(move || {
            run_async_writer_loop(rx, CAPACITY, pending_ref, dropped_ref, |_line| {
                std::thread::sleep(SINK_COST);
            });
        });

        let producer_tx = tx.clone();
        let producer = scope.spawn(move || {
            // No pacing at all: the callback thread this stands in for
            // wins every race for a freed slot it is ALLOWED to enter.
            while producing_ref.load(Ordering::Relaxed) {
                enqueue_async_into(
                    &producer_tx,
                    gate_ref,
                    pending_ref,
                    dropped_ref,
                    "raw= mouse move".to_owned(),
                );
            }
        });

        let saturate_by = Instant::now() + Duration::from_secs(10);
        while dropped.unnamed() < SATURATION_EVIDENCE && Instant::now() < saturate_by {
            std::thread::yield_now();
        }
        let saturated = dropped.unnamed() >= SATURATION_EVIDENCE;

        let started = Instant::now();
        let acked = drain_and_shutdown_into(&tx, &gate, DEADLINE);
        let elapsed = started.elapsed();

        keep_producing.store(false, Ordering::Relaxed);
        let _ = producer.join();
        // Cleanup only: with the producer stopped the queue drains, so a
        // BLOCKING send is safe here and it is what lets the writer loop
        // return on the un-fixed tree (where our sentinel never landed).
        // Errors are expected on the fixed tree - the writer has already
        // returned and dropped the receiver.
        let (cleanup_ack, _cleanup_rx) = std::sync::mpsc::channel::<()>();
        let _ = tx.send(AsyncRecord::Shutdown(cleanup_ack));
        (acked, elapsed, saturated)
    });

    assert!(
        saturated,
        "harness: the producer must have kept the queue full before \
         teardown started, otherwise the sentinel wins a slot for free and \
         nothing about the starvation is exercised"
    );
    assert!(
        acked,
        "the drain must be acknowledged while a producer keeps saturating \
         the queue. The sink here is slow but working, so nothing is \
         wedged: a failure means every slot the writer freed went back to \
         the producer before the teardown thread retried, the sentinel \
         never fit, and the process exits with the caller's backlog \
         undrained. Close the queue to new lines before polling for \
         sentinel space"
    );
    assert!(
        elapsed < DEADLINE,
        "an acknowledged drain must return well inside its deadline; a \
         starved sentinel took {elapsed:?} of {DEADLINE:?}"
    );
}

// -----------------------------------------------------------------------
// Structural: production must be wired to the same gate.
// -----------------------------------------------------------------------

/// The runtime tests above drive the parameterised halves; this pins that
/// PRODUCTION passes the one process-wide gate to both sides. Handing the
/// producer a different gate from the one teardown closes would leave
/// every runtime test green while shipping the starvation unchanged.
#[test]
fn production_wires_both_sides_of_the_queue_to_the_same_gate() {
    let enqueue = scan_fn_body(
        "src/rust/diag.rs",
        "pub fn enqueue_async(message: String) {",
    );
    assert!(
        enqueue.code.contains("ASYNC_GATE"),
        "enqueue_async must consult the process-wide `ASYNC_GATE`; a \
         producer that skips it keeps refilling the queue through \
         teardown. Offending function body:\n{}",
        enqueue.raw
    );

    let drain = scan_fn_body(
        "src/rust/diag.rs",
        "pub fn drain_and_shutdown(deadline: Duration) -> bool {",
    );
    assert!(
        drain.code.contains("ASYNC_GATE"),
        "drain_and_shutdown must close the SAME process-wide `ASYNC_GATE` \
         that `enqueue_async` consults; a second instance closes nothing \
         the producer can see. Offending function body:\n{}",
        drain.raw
    );

    let sender = scan_fn_body(
        "src/rust/diag.rs",
        "pub(crate) fn enqueue_async_into_after<H>(",
    );
    let admits_at = sender
        .code
        .find("gate.admits()")
        .expect("enqueue_async_into_after must consult the gate at all");
    let reserve_at = sender
        .code
        .find("take_unbound")
        .expect("enqueue_async_into_after must still reserve the drop count");
    assert!(
        admits_at < reserve_at,
        "the gate check must come BEFORE the drop reservation: a refused \
         line that has already emptied the unbound counter takes a \
         pre-teardown gap down with it, and no marker ever names it. \
         Offending function body:\n{}",
        sender.raw
    );
}
