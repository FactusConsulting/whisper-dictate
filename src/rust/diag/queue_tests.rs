//! queue diagnostic regressions.

use super::{diag_test_lock, flood_a_stalled_async_queue, scan_fn_body};
use crate::diag::DropLedger;
use std::sync::atomic::AtomicUsize;

/// The regression test for the shed-visibility fix, plus the marker's
/// POSITION within the drained trace.
///
/// Un-fixed behaviour, two distinct regressions:
///
/// * `if tx.try_send(message).is_ok()` (no accounting at all): the
///   flood is accepted-or-discarded with no bookkeeping, `shed` stays 0
///   and the sink sees records only — no marker line at all.
/// * Writer reads a shared counter at write time
///   shape): the marker is emitted before the FIRST record it dequeues.
///   Those `CAPACITY` records were accepted BEFORE the gap, so the log
///   claims the trace broke up to a whole backlog earlier than it did
///   `recorded.first()` is the marker instead of `flood #0`.
#[test]
fn bounded_async_queue_sheds_and_reports_a_coalesced_dropped_marker() {
    // Belt-and-braces: the pipeline halves under test are parameterised
    // away from the global writer slot and the LEVEL atomic, but a
    // future rework that reached for either would otherwise flake
    // against the rest of the suite.
    let _guard = diag_test_lock();

    const CAPACITY: usize = 8;
    const OVERFLOW: usize = 200;
    let run = flood_a_stalled_async_queue(CAPACITY, OVERFLOW);
    let recorded = &run.recorded;

    assert!(
        run.flood_elapsed < std::time::Duration::from_secs(2),
        "enqueue must never block on a full queue (the whole \
         WH_KEYBOARD_LL callback budget is a few ms); flooding {} \
         records took {:?}",
        CAPACITY + OVERFLOW,
        run.flood_elapsed
    );
    assert_eq!(
        run.accepted, CAPACITY,
        "a capacity-{CAPACITY} channel with no consumer must accept \
         exactly {CAPACITY} records and shed the rest; accepted={}",
        run.accepted
    );
    assert_eq!(
        run.shed,
        OVERFLOW as u64,
        "a full queue must ACCOUNT for what it shed; after flooding {} \
         records into a capacity-{CAPACITY} channel exactly {OVERFLOW} \
         must be counted as dropped, got {}. dropped=0 means the \
         drop is silent again and `gui-diagnostic.log` cannot \
         distinguish a quiet callback from a shed burst",
        CAPACITY + OVERFLOW,
        run.shed
    );

    // The memory bound, restated on the output side: the writer can
    // only ever have held `capacity` records, plus the one marker.
    assert_eq!(
        recorded.len(),
        CAPACITY + 1,
        "the writer must emit exactly the {CAPACITY} surviving records \
         plus ONE marker; saw {} lines: {recorded:?}",
        recorded.len()
    );

    let markers: Vec<&String> = recorded
        .iter()
        .filter(|line| line.contains("[diag-async] dropped="))
        .collect();
    assert_eq!(
        markers.len(),
        1,
        "drops must be reported as ONE coalesced marker, never one line \
         per drop (that would be unbounded write amplification against \
         the same stalled sink that caused the drops) and never zero \
         (that is the silent shed this test exists to prevent); got \
         {markers:?} out of {recorded:?}"
    );
    assert_eq!(
        markers[0],
        &crate::diag::async_dropped_marker(run.shed, CAPACITY),
        "the marker must name the exact shed count and the capacity that \
         was exceeded, so a log reader can size the gap"
    );
    // POSITION is the point: `Full` sheds the NEWEST record, so every
    // record still in the queue was accepted BEFORE the gap and must be
    // written before the marker. A marker at the front would date the
    // gap a whole backlog too early for a reader correlating `t=<ms>`
    // prefixes against the moment PTT died.
    let (survivors, tail) = recorded.split_at(CAPACITY);
    let expected: Vec<String> = (0..CAPACITY).map(|i| format!("flood #{i}")).collect();
    assert_eq!(
        survivors.iter().map(String::as_str).collect::<Vec<_>>(),
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "every ACCEPTED record must reach the sink, in order, BEFORE the \
         marker for a gap that happened after they were accepted; \
         recorded={recorded:?}"
    );
    assert_eq!(
        tail,
        [markers[0].clone()],
        "the marker must sit at the queue position of the gap — after \
         the records the queue had already accepted, not ahead of them; \
         recorded={recorded:?}"
    );
    assert_eq!(
        run.pending_after, 0,
        "every record the queue ACCEPTED must have reached the sink; a \
         non-zero pending count would stall `flush_async_for_tests`"
    );
    assert_eq!(
        run.dropped_after, 0,
        "emitting the marker must RESET the counter, so the next burst \
         reports its own size and not a running total"
    );
    assert_eq!(
        run.unnamed_after, 0,
        "every shed record must end up NAMED by a marker; a residue here \
         is a gap the log never told the reader about"
    );
}

/// A gap at the very END of a trace must still reach the log.
///
/// This is the wedge case the whole accounting exists for: PTT dies,
/// the queue sheds the tail of the burst, and then NOTHING else ever
/// happens. The writer's sender is process-wide and never dropped, so a
/// writer parked in a plain blocking `recv()` never wakes again and the
/// final shed burst stays silent forever — exactly the run an operator
/// would be reading the log to explain
/// 3667524121).
///
/// The counter is bumped directly rather than through
/// `enqueue_async_into` on purpose: a shed REQUIRES a full queue, and a
/// live writer drains the queue, so "a drop lands while the writer is
/// parked on an empty queue" cannot be scheduled deterministically from
/// the producer side. The bump is byte-for-byte what
/// `enqueue_async_into` does on `TrySendError::Full`, and the contract
/// under test is the writer's: an outstanding count must surface with
/// no record to carry it.
///
/// Un-fixed behaviour (`while let Ok(line) = rx.recv()`): the writer
/// parks forever, the polling loop below times out, and `observed` is
/// empty — the trailing `close_burst_with_pending_drops` only runs once
/// `tx` is dropped, which is why `observed` is snapshotted BEFORE that.
#[test]
fn a_shed_burst_that_ends_the_trace_still_reaches_the_log() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const SHED: u64 = 7;

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);
    let recorded = std::sync::Mutex::new(Vec::<String>::new());
    let expected = crate::diag::async_dropped_marker(SHED, CAPACITY);

    // Everything that could panic happens AFTER the scope: a failing
    // assertion inside would unwind while the writer is parked and the
    // implicit join would hang forever.
    let observed = std::thread::scope(|scope| {
        scope.spawn(|| {
            crate::diag::run_async_writer_loop(rx, CAPACITY, &pending, &dropped, |line| {
                recorded
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(line.to_owned());
            });
        });

        // The producer's `Full` branch, with no record before it and
        // none after it.
        dropped.shed_for_tests(SHED);

        // Generous by ~20x against the writer's park interval so a
        // loaded CI box cannot turn a pass into a flake; a healthy run
        // leaves this loop in well under a second.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen: Vec<String> = Vec::new();
        while std::time::Instant::now() < deadline {
            seen = recorded
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            if !seen.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Snapshot taken; releasing the sender lets the loop return so
        // the scope can join it.
        drop(tx);
        seen
    });

    assert_eq!(
        observed,
        vec![expected],
        "a burst shed after the LAST record must still be reported: the \
         writer's sender is never dropped, so a writer that only wakes \
         on the next record never reports the gap that ended the trace. \
         Saw {observed:?}"
    );
}

/// A healthy, drained pipeline must report nothing shed. Names
/// `async_dropped_count` so the accessor has a test that exercises it,
/// and pins the other half of the contract: the marker is an anomaly
/// signal, so ordinary traffic through the real process-wide writer
/// must never produce one.
#[test]
fn async_dropped_count_is_zero_on_a_healthy_drained_pipeline() {
    let _guard = diag_test_lock();
    crate::diag::ensure_async_writer();
    crate::diag::log_async!("[test] healthy async pipeline line");
    crate::diag::flush_async_for_tests();
    assert_eq!(
        crate::diag::async_dropped_count(),
        0,
        "a handful of records through an idle capacity-{} queue must not \
         shed anything; a non-zero count here means the accounting is \
         counting successful sends",
        crate::diag::ASYNC_QUEUE_CAPACITY
    );
}

/// Structural companion to the runtime test above: the runtime test
/// drives the parameterised halves, this one pins that PRODUCTION is
/// actually wired to them. A regression that reverted `enqueue_async`
/// to `if tx.try_send(message).is_ok()`, or that spawned a writer loop
/// which never consults the drop counter, would leave the runtime test
/// green while shipping silent drops again.
#[test]
fn production_async_queue_is_wired_to_the_drop_accounting() {
    let enqueue = scan_fn_body(
        "src/rust/diag/queue.rs",
        "pub fn enqueue_async(message: String) {",
    );
    assert!(
        enqueue.code.contains("enqueue_async_into"),
        "enqueue_async must route through `enqueue_async_into` so the \
         production path and the tested path are the same code. \
         Offending function body:\n{}",
        enqueue.raw
    );

    let delegate = scan_fn_body(
        "src/rust/diag/queue.rs",
        "pub(crate) fn enqueue_async_into(",
    );
    assert!(
        delegate.code.contains("enqueue_async_into_after"),
        "enqueue_async_into must delegate to `enqueue_async_into_after` \
         with an empty seam, so the function `diag_tests` drives the \
         reservation-vs-sentinel race through IS the production sender \
         and not a parallel copy. Offending function body:\n{}",
        delegate.raw
    );

    let sender = scan_fn_body(
        "src/rust/diag/queue.rs",
        "pub(crate) fn enqueue_async_into_after<H>(",
    );
    assert!(
        sender.code.contains("TrySendError::Full"),
        "enqueue_async_into must match on `TrySendError::Full` — an \
         `is_ok()` shortcut collapses `Full` and `Disconnected` into one \
         silent discard and loses the drop accounting. Offending \
         function body:\n{}",
        sender.raw
    );
    assert!(
        sender.code.contains("dropped.shed("),
        "enqueue_async_into_after must bump the drop LEDGER on a full \
         queue, otherwise the writer has nothing to report. Offending \
         function body:\n{}",
        sender.raw
    );
    assert!(
        !sender.code.contains("tx.send("),
        "enqueue_async_into must never use the BLOCKING `send` — it runs \
         on the Windows LL-hook callback thread, where parking on a slow \
         sink is the wedge this queue exists to prevent. Offending \
         function body:\n{}",
        sender.raw
    );
    assert!(
        sender.code.contains("take_unbound") && sender.code.contains("drops_before"),
        "enqueue_async_into_after must bind the outstanding shed count to \
         the record it is accepting (an `AsyncRecord::Line` carrying \
         `drops_before`), \
         not leave it for the writer to read at write time — `Full` sheds \
         the NEWEST record, so a writer-side read dates the gap ahead of \
         the older records still queued. Offending function body:\n{}",
        sender.raw
    );

    let writer = scan_fn_body(
        "src/rust/diag/writer.rs",
        "pub(crate) fn run_async_writer_loop<F>(",
    );
    assert!(
        writer.code.contains("recv_timeout"),
        "run_async_writer_loop must park with `recv_timeout`, never a \
         plain blocking `recv()`: the process-wide sender is never \
         dropped, so a writer parked on `recv()` never wakes to report a \
         burst shed after the last record — the wedge case the drop \
         accounting exists for. Offending function body:\n{}",
        writer.raw
    );
    assert!(
        writer.code.contains("BurstState"),
        "run_async_writer_loop must carry the overload-episode state \
         across iterations: emitting a marker for every record that \
         carries a non-zero count is nearly one marker per surviving \
         record under a sustained overload, which doubles the write \
         volume against the sink that was already too slow.\
         Offending function body:\n{}",
        writer.raw
    );

    let install = scan_fn_body("src/rust/diag/startup.rs", "pub fn ensure_async_writer() {");
    assert!(
        install.code.contains("run_async_writer_loop"),
        "the production writer thread must run `run_async_writer_loop` \
         so it emits the coalesced dropped marker; an inline `while let \
         Ok(line) = rx.recv()` loop would silently skip the accounting. \
         Offending function body:\n{}",
        install.raw
    );
    assert!(
        install.code.contains("ASYNC_DROPPED"),
        "the production writer thread must be handed the process-wide \
         drop counter that `enqueue_async` bumps; handing it a different \
         counter would report zero forever. Offending function body:\n{}",
        install.raw
    );
}
