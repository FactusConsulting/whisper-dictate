//! shutdown protocol diagnostic regressions.

use super::{diag_test_lock, release_gate, scan_fn_body};
use crate::diag::DropLedger;
use crate::diag_shutdown_gate::ShutdownGate;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

/// The drain acknowledgement must not wait on traffic queued after the
/// sentinel.
///
/// The previous round bounded the formerly infinite sweep by COUNT
/// (`capacity`). A count is the wrong currency for a deadline: against a
/// slow-but-functional sink, a queue's worth of post-sentinel records
/// can cost far more than the caller's 500 ms, so a `main` on its way
/// out times out and warns the operator that the tee file is short - on
/// a run where every record the request covered was already durable
/// (FIFO puts them ahead of the sentinel). The fix acks first and sweeps
/// afterwards, on borrowed time.
///
/// ## The deterministic seam (no sleeps, no scheduling race)
///
/// The whole queue is built BEFORE the writer thread starts, so the
/// ordering under test is chosen rather than raced for:
///
/// `[pre-sentinel record] [Shutdown] [post-sentinel x5] [Shutdown]`
///
/// The sink writes the first line instantly and then parks forever. So
/// "the ack arrived" can only mean "the ack did not wait for a single
/// post-sentinel write" - a slow sink modelled as an infinitely slow one,
/// which is the same claim without a sleep in it.
///
/// The trailing second sentinel pins what the sweep is still FOR: a
/// concurrent drainer must still be acked, or its `recv_timeout` reports
/// a spurious failure.
///
/// Un-fixed behaviour (sweep-then-ack, with any budget): the sweep's
/// first post-sentinel write parks on the gate, the ack never arrives,
/// and this fails on "the drain must be acknowledged without waiting for
/// post-sentinel traffic".
#[test]
fn the_drain_ack_does_not_wait_for_post_sentinel_traffic() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 8;
    const DEADLINE: std::time::Duration = std::time::Duration::from_millis(250);

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);

    let queue_line = |message: String| {
        pending.fetch_add(1, Ordering::Relaxed);
        tx.send(crate::diag::AsyncRecord::Line {
            drops_before: 0,
            message,
        })
        .expect("the pre-built queue has room");
    };

    // The only record the drain request covers.
    queue_line("pre-sentinel record".to_owned());
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
    tx.send(crate::diag::AsyncRecord::Shutdown(ack_tx))
        .expect("the pre-built queue has room for the sentinel");
    // The unjoinable callback thread that keeps firing through teardown.
    for i in 0..(CAPACITY - 3) {
        queue_line(format!("post-sentinel callback trace #{i}"));
    }
    // A second concurrent drainer, behind all of it.
    let (second_ack_tx, second_ack_rx) = std::sync::mpsc::channel::<()>();
    tx.send(crate::diag::AsyncRecord::Shutdown(second_ack_tx))
        .expect("the pre-built queue has room for the second sentinel");
    drop(tx);

    let gate: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

    // Observe inside, assert outside: a panic in the scope would unwind
    // while the writer is parked on the gate, and `thread::scope` would
    // block forever joining it - an expected FAILURE turned into a HANG.
    let (acked, elapsed, second_acked) = std::thread::scope(|scope| {
        let (pending_ref, dropped_ref, gate_ref) = (&pending, &dropped, &gate);
        scope.spawn(move || {
            let mut written = 0usize;
            crate::diag::run_async_writer_loop(rx, CAPACITY, pending_ref, dropped_ref, |_line| {
                written += 1;
                // Line 1 is the pre-sentinel record. Everything after it
                // is post-sentinel traffic, and the sink stalls on it.
                if written > 1 {
                    let (lock, cv) = gate_ref;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = cv.wait(released).unwrap();
                    }
                }
            });
        });

        let started = std::time::Instant::now();
        let acked = ack_rx.recv_timeout(DEADLINE).is_ok();
        let elapsed = started.elapsed();

        // Let the writer out so the scope can join it: on the un-fixed
        // tree it is still parked mid-sweep.
        release_gate(&gate);
        let second_acked = second_ack_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();
        (acked, elapsed, second_acked)
    });

    assert!(
        acked,
        "the drain must be acknowledged without waiting for post-sentinel \
         traffic: every record the request covered is ordered AHEAD of the \
         sentinel and was already written, so making the caller pay for a \
         queue's worth of younger records only burns its {DEADLINE:?} \
         budget and reports a lost-records warning on a run that lost \
         nothing"
    );
    assert!(
        elapsed < DEADLINE,
        "the ack must land well inside the caller's budget; it took \
         {elapsed:?} of {DEADLINE:?}"
    );
    assert!(
        second_acked,
        "a second concurrent drainer's sentinel must still be found by the \
         post-ack sweep and acknowledged; dropping it makes that drainer's \
         `recv_timeout` report a spurious failure"
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "the sweep must still write (and count out) the post-sentinel \
         records once the sink is moving again"
    );
}

/// A drop reservation taken before
/// the shutdown sentinel must be named BEFORE the drain is acknowledged.
///
/// ## The race
///
/// The rdev / raw-hook callback thread is unjoinable and keeps firing all
/// through teardown. One of those callbacks can run
/// `DropLedger::take_unbound` - emptying the counter that says "a gap
/// happened" - and only then have its record accepted, by which time
/// `drain_and_shutdown_into` has already slipped its sentinel into the
/// queue ahead of it. The shutdown arm reads an unbound counter of zero,
/// finds nothing to report, and acknowledges. `main` is entitled to exit
/// the instant that ack lands, so the post-ack sweep that would have
/// found the stranded record is not guaranteed to run: a gap that
/// happened BEFORE the drain request, and the trace line that resumed
/// after it, are lost on precisely the exit the drain exists to make
/// readable.
///
/// ## The deterministic seam (no threads, no sleeps, no timing)
///
/// [`crate::diag::enqueue_async_into_after`] runs its closure in the one
/// window that matters - after the reservation is taken, before the
/// record is offered to the channel - so the sentinel is enqueued at
/// exactly the point the test needs, by construction rather than by racing
/// for it. The writer loop then runs INLINE on this thread, so "was the
/// gap named before the ack?" is answered by a `try_recv` on the ack
/// channel from inside the sink: same thread, no window for the answer to
/// change under the observation.
///
/// A broken implementation (`close_burst_with_pending_drops` in the shutdown
/// arm, i.e. reading only the unbound counter): the marker is emitted by
/// the post-ack sweep instead, so it is recorded with `acked == true` and
/// this fails on "must be named BEFORE the drain is acknowledged".
#[test]
fn a_drop_reserved_before_the_sentinel_is_named_before_the_drain_is_acked() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const SHED: usize = 3;

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);

    // Fill the queue, then shed: a gap that happened BEFORE any teardown.
    for i in 0..CAPACITY {
        crate::diag::enqueue_async_into(&tx, &admission, &pending, &dropped, format!("pre #{i}"));
    }
    for i in 0..SHED {
        crate::diag::enqueue_async_into(&tx, &admission, &pending, &dropped, format!("shed #{i}"));
    }
    assert_eq!(
        (dropped.unbound(), dropped.unnamed()),
        (SHED as u64, SHED as u64),
        "harness: the flood must have shed exactly {SHED} records with \
         nothing having named them yet"
    );

    // Make room the way the writer would, so the sentinel and the
    // producer's record both fit. `pending` is corrected by hand because
    // these two never reach the sink.
    for _ in 0..2 {
        rx.try_recv().expect("harness: two records to consume");
        pending.fetch_sub(1, Ordering::Relaxed);
    }

    // THE RACE, made deterministic: the producer takes the reservation,
    // the drain queues its sentinel inside that window, and the
    // producer's record is accepted behind it.
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
    crate::diag::enqueue_async_into_after(
        &tx,
        &admission,
        &pending,
        &dropped,
        "resumed after the gap".to_owned(),
        || {
            tx.send(crate::diag::AsyncRecord::Shutdown(ack_tx))
                .expect("harness: the queue has room for the sentinel");
        },
    );
    assert_eq!(
        (dropped.unbound(), dropped.unnamed()),
        (0, SHED as u64),
        "harness: the reservation must have LEFT the unbound counter (that \
         is the whole race) while the ledger still knows the gap is unnamed"
    );

    // Inline: the shutdown arm returns from the loop, so there is no
    // thread to join and nothing can be written after the snapshot.
    let mut recorded: Vec<(String, bool)> = Vec::new();
    let mut acked = false;
    crate::diag::run_async_writer_loop(rx, CAPACITY, &pending, &dropped, |line| {
        acked = acked || ack_rx.try_recv().is_ok();
        recorded.push((line.to_owned(), acked));
    });

    let named_before_ack: Vec<&String> = recorded
        .iter()
        .filter(|(line, after_ack)| line.starts_with("[diag-async]") && !after_ack)
        .map(|(line, _)| line)
        .collect();
    assert_eq!(
        named_before_ack,
        vec![&crate::diag::async_dropped_marker(SHED as u64, CAPACITY)],
        "the gap must be named BEFORE the drain is acknowledged. The \
         reservation was taken before the sentinel was queued, so it is \
         not post-sentinel traffic the ack is allowed to skip - and `main` \
         may exit the moment the ack lands, so a marker written by the \
         post-ack sweep is a marker that may never exist. Recorded \
         (line, already-acked): {recorded:?}"
    );
    assert!(
        acked,
        "harness: the drain must have been acknowledged at all"
    );
    assert_eq!(
        (dropped.unbound(), dropped.unnamed()),
        (0, 0),
        "the ledger must be empty once the writer has stopped: a residue \
         is an unreported gap, and naming the same gap twice would show up \
         as an extra marker above"
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "every record the queue accepted must still be counted out of \
         `pending`, including the one the sweep found behind the sentinel"
    );
}

/// Structural companion: production must be wired to the sentinel-based
/// drain, and the writer loop must flush the backlog before it acks. A
/// regression that reverted the sentinel to "stop immediately" would
/// leave the injected-core tests in `entrypoint_tests` green while
/// shipping an exit path that still discards the queue.
#[test]
fn production_async_writer_drains_before_it_stops() {
    let drain = scan_fn_body(
        "src/rust/diag/shutdown.rs",
        "pub fn drain_and_shutdown(deadline: Duration) -> bool {",
    );
    assert!(
        drain.code.contains("drain_and_shutdown_into"),
        "drain_and_shutdown must route through `drain_and_shutdown_into` \
         so production and the tested path are the same code. Offending \
         function body:\n{}",
        drain.raw
    );

    let delegate = scan_fn_body(
        "src/rust/diag/shutdown.rs",
        "pub(crate) fn drain_and_shutdown_into(",
    );
    assert!(
        delegate.code.contains("drain_and_shutdown_into_after"),
        "drain_and_shutdown_into must delegate to \
         `drain_and_shutdown_into_after` with an empty seam, so the \
         function `diag_tests` drives the freed-slot-vs-producer race \
         through IS the production drain and not a parallel copy. \
         Offending function body:\n{}",
        delegate.raw
    );

    let sender = scan_fn_body(
        "src/rust/diag/shutdown.rs",
        "pub(crate) fn drain_and_shutdown_into_after<H>(",
    );
    assert!(
        sender.code.contains("gate.close()"),
        "drain_and_shutdown_into_after must CLOSE the admission gate \
         before it starts polling for sentinel space. Polling alone loses \
         every freed slot to the unjoinable callback producer that keeps \
         firing through teardown, so the sentinel starves for the whole \
         deadline against a writer that was never wedged.\
         Offending function body:\n{}",
        sender.raw
    );
    assert!(
        sender.code.contains("try_send"),
        "drain_and_shutdown_into must poll `try_send`: the queue is \
         bounded, so a blocking `send` behind a stalled writer would hang \
         process exit - the very thing the deadline exists to prevent \
         (`SyncSender::send_timeout` is still unstable). Offending \
         function body:\n{}",
        sender.raw
    );
    assert!(
        sender.code.contains("recv_timeout"),
        "drain_and_shutdown_into must wait for the ack with `recv_timeout`, \
         never a bare `recv` - a wedged writer must not pin teardown. \
         Offending function body:\n{}",
        sender.raw
    );

    let loop_body = scan_fn_body(
        "src/rust/diag/writer.rs",
        "pub(crate) fn run_async_writer_loop<F>(",
    );
    assert!(
        loop_body.code.contains("drain_and_ack_shutdown"),
        "the writer loop's Shutdown arm must route through \
         `drain_and_ack_shutdown` so the drain semantics live in one \
         place. Offending function body:\n{}",
        loop_body.raw
    );

    let drain_arm = scan_fn_body("src/rust/diag/writer.rs", "fn drain_and_ack_shutdown<F>(");
    assert!(
        drain_arm.code.contains("try_recv"),
        "on the Shutdown sentinel the writer must still sweep what is \
         queued behind it (`try_recv`, never `recv`) - a full-at-sentinel \
         queue and a second drainer's sentinel both live there. Since \
         the sweep runs AFTER the ack, \
         off the caller's deadline, but it must not disappear. Offending \
         function body:\n{}",
        drain_arm.raw
    );
    assert!(
        drain_arm
            .code
            .contains("close_burst_with_every_unnamed_drop"),
        "the drain must CLOSE any open overload episode before acking \
         (the `BurstState` slot), and it must close it on `every unnamed \
         drop` terms rather than `pending drops` terms. Two failures ride \
         on that word: a drain landing mid-burst would take the process \
         down with the episode's summary marker unwritten, and a producer \
         holding a drop reservation across the sentinel \
         would leave the unbound counter reading zero \
         so the gap is never named at all. Offending function body:\n{}",
        drain_arm.raw
    );
}
