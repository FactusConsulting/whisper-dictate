//! writer diagnostic regressions.

use super::{diag_test_lock, drive_a_saturated_async_queue, reported_drop_counts};
use crate::diag::DropLedger;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A queue that
/// stays saturated must produce a marker count that scales with the
/// number of overload EPISODES, not with the number of surviving records.
///
/// Un-fixed behaviour (a marker for every record carrying a non-zero
/// count) with the constants below: 24 markers for 28 surviving records
/// 0.86 markers per record, i.e. the trace is very nearly half marker
/// noise — and the last marker names 5, not the 120 records actually shed.
/// Fixed: 2 markers (one opening the episode, one summarising it) no
/// matter how long the overload lasts.
#[test]
fn a_saturated_queue_coalesces_its_markers_across_the_whole_burst() {
    // The halves under test are parameterised away from the global
    // writer slot and the LEVEL atomic, but hold the lock anyway so a
    // future rework that reached for either cannot flake the suite.
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const ROUNDS: usize = 24;
    const SHED_PER_ROUND: usize = 5;
    /// Every episode costs at most an opening marker and a closing
    /// summary. The point of the bound is that it does NOT contain
    /// `ROUNDS`.
    const MAX_MARKERS: usize = 2;

    let run = drive_a_saturated_async_queue(CAPACITY, ROUNDS, SHED_PER_ROUND);

    assert!(
        !run.handshake_failed,
        "the lock-step handshake timed out, so the run never reached the \
         saturated steady state this test measures; recorded={:?}",
        run.recorded
    );

    let markers = reported_drop_counts(&run.recorded);
    let records: Vec<&String> = run
        .recorded
        .iter()
        .filter(|line| !line.starts_with("[diag-async]"))
        .collect();

    let expected_records: Vec<String> = (0..CAPACITY)
        .map(|i| format!("seed #{i}"))
        .chain((0..ROUNDS).map(|i| format!("burst #{i}")))
        .collect();
    assert_eq!(
        records.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        expected_records
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        "the lock-step harness must produce exactly the prefilled seeds \
         plus one surviving record per round, in order — otherwise the \
         marker count below is being compared against a different run; \
         recorded={:?}",
        run.recorded
    );

    assert!(
        markers.len() <= MAX_MARKERS,
        "a sustained overload must be announced ONCE per episode, not once \
         per surviving record: got {} markers for {} surviving records \
         ({:.2} markers/record). Every dequeue frees exactly one slot, the \
         producer refills it and sheds more while the writer writes, so a \
         per-record marker doubles the write volume against the sink that \
         was already too slow. markers={markers:?}",
        markers.len(),
        records.len(),
        markers.len() as f64 / records.len() as f64,
    );
    assert!(
        !markers.is_empty(),
        "coalescing must not become silence — an overload that shed {} \
         records has to be visible in the log; recorded={:?}",
        ROUNDS * SHED_PER_ROUND,
        run.recorded
    );

    // Position, unchanged from the previous round: the gap opens on the
    // first record the producer enqueued against a full queue, which is
    // the record after the `CAPACITY` seeds and the first burst record.
    let first_marker_at = run
        .recorded
        .iter()
        .position(|line| line.starts_with("[diag-async]"))
        .expect("at least one marker");
    assert_eq!(
        first_marker_at,
        CAPACITY + 1,
        "the opening marker must still sit at the QUEUE POSITION of the \
         gap — after every record accepted before it, immediately ahead of \
         the first record accepted after it. recorded={:?}",
        run.recorded
    );

    assert_eq!(
        markers.last().copied(),
        Some((ROUNDS * SHED_PER_ROUND) as u64),
        "coalescing may delay a count, never lose one: the closing marker \
         must name every record the burst shed ({} of them). markers={markers:?}",
        ROUNDS * SHED_PER_ROUND
    );
    assert_eq!(
        run.pending_after, 0,
        "every record the queue ACCEPTED must still have reached the sink"
    );
}

/// The episode state machine's exact output, driven synchronously so the
/// wording and the position of both markers are pinned without a thread
/// in sight.
///
/// Records are handed to the writer pre-built rather than through
/// `enqueue_async_into` because the interesting sequence — shed, shed,
/// recovered, shed again — needs the carried counts chosen, and a real
/// producer can only produce them by actually saturating a queue (which
/// is what the concurrent test above does).
///
/// Un-fixed behaviour: a marker before `gap opens` AND before `still
/// shedding`, and no summary at all when the queue recovers.
#[test]
fn an_overload_burst_is_summarised_once_when_the_queue_catches_up() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 64;
    let clear_run = crate::diag::ASYNC_BURST_CLEAR_RUN;

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);

    let mut queued: Vec<(u64, String)> = vec![
        (0, "before".to_owned()),
        (3, "gap opens".to_owned()),
        (4, "still shedding".to_owned()),
    ];
    queued.extend((0..clear_run).map(|i| (0, format!("recovered #{i}"))));
    queued.push((2, "second burst".to_owned()));
    // The ledger has to agree with the hand-built records: a real
    // producer that bound 3 + 4 + 2 drops to these records would have
    // counted all nine as shed-but-unnamed first. Stating it here keeps
    // the "every shed record ends up named exactly once" invariant
    // assertable at the bottom.
    dropped.carried_for_tests(9);
    for (drops_before, message) in &queued {
        pending.fetch_add(1, Ordering::Relaxed);
        tx.send(crate::diag::AsyncRecord::Line {
            drops_before: *drops_before,
            message: message.clone(),
        })
        .expect("the capacity-64 queue accepts the whole script");
    }
    // Closed before the loop runs, so it drains and returns inline: no
    // thread to join, no parked writer for an assertion to unwind into.
    drop(tx);

    let mut recorded: Vec<String> = Vec::new();
    crate::diag::run_async_writer_loop(rx, CAPACITY, &pending, &dropped, |line| {
        recorded.push(line.to_owned());
    });

    let mut expected: Vec<String> = vec![
        "before".to_owned(),
        // The episode opens naming only what THIS record carried...
        crate::diag::async_dropped_marker(3, CAPACITY),
        "gap opens".to_owned(),
        // ...and the next shed is folded in silently.
        "still shedding".to_owned(),
    ];
    expected.extend((0..clear_run - 1).map(|i| format!("recovered #{i}")));
    // `clear_run` clean records in a row means the queue caught up, so the
    // episode closes with ONE summary naming 3 + 4.
    expected.push(crate::diag::async_burst_summary_marker(7, CAPACITY));
    expected.push(format!("recovered #{}", clear_run - 1));
    // A fresh gap after the recovery is a NEW episode, announced again.
    expected.push(crate::diag::async_dropped_marker(2, CAPACITY));
    expected.push("second burst".to_owned());
    // Disconnected with an episode still open: it is closed and summarised
    // rather than left unreported.
    expected.push(crate::diag::async_burst_summary_marker(2, CAPACITY));

    assert_eq!(
        recorded, expected,
        "one marker opens an overload episode, one summary closes it once \
         {clear_run} consecutive records arrive with nothing shed ahead of \
         them, and a later gap opens a new episode. Anything else is either \
         per-record amplification or a lost count."
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "every record must still be counted out of `pending` after it is \
         written"
    );
    assert_eq!(
        dropped.unbound(),
        0,
        "closing an episode must reset the shared counter"
    );
    assert_eq!(
        dropped.unnamed(),
        0,
        "each of the 9 carried drops must be named exactly once - a \
         residue is an unreported gap, a negative-turned-saturated ledger \
         would be a double report"
    );
}

/// The interaction between [`BurstState`] and the drain:
/// an exit that lands MID-EPISODE must still write the episode summary.
///
/// A burst is announced once when it opens and summarised once when the
/// queue catches up. A drain arriving before the queue caught up is the
/// LAST thing that will ever happen on this writer, so if the shutdown
/// arm just acks and returns, the episode's closing summary is never
/// written and the tee file's final word about a wedged sink is the
/// episode's OPENING count instead of its total - on precisely the
/// crash-adjacent exit both mechanisms exist to make readable.
///
/// Deterministic and inline: records are handed to the writer pre-built
/// so the carried counts are chosen rather than raced for, and the
/// sentinel is already in the channel before the loop starts, so there
/// is no thread to join and no timing to assert.
///
/// Un-fixed behaviour (drain arm without
/// `close_burst_with_pending_drops`): the summary line is missing and
/// the outstanding counter is left non-zero.
#[test]
fn a_drain_landing_mid_burst_still_writes_the_episode_summary() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 16;
    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);

    // One record carrying a shed count opens the episode...
    pending.fetch_add(1, Ordering::Relaxed);
    tx.send(crate::diag::AsyncRecord::Line {
        drops_before: 7,
        message: "after the gap".to_owned(),
    })
    .expect("the queue accepts the opening record");
    // ...and the drain arrives while it is still open, far short of the
    // ASYNC_BURST_CLEAR_RUN clean records that would close it normally.
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
    tx.send(crate::diag::AsyncRecord::Shutdown(ack_tx))
        .expect("the queue accepts the sentinel");
    // Plus a shed that no record will ever carry - the drain is the last
    // chance to report it. `carried_for_tests(7)` is the other half of
    // the ledger state a real producer would have left behind for the
    // hand-built record above.
    dropped.carried_for_tests(7);
    dropped.shed_for_tests(3);

    let mut recorded: Vec<String> = Vec::new();
    crate::diag::run_async_writer_loop(rx, CAPACITY, &pending, &dropped, |line| {
        recorded.push(line.to_owned());
    });

    assert_eq!(
        recorded,
        vec![
            crate::diag::async_dropped_marker(7, CAPACITY),
            "after the gap".to_owned(),
            crate::diag::async_burst_summary_marker(10, CAPACITY),
        ],
        "the shutdown arm must close the open episode, folding in the 3 \
         records no accepted record could carry, so the tee file names \
         the episode TOTAL before the process goes away"
    );
    assert!(
        ack_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok(),
        "the drain must still be acknowledged after the summary is written"
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "the drained record must still be counted out of `pending`"
    );
    assert_eq!(
        dropped.unbound(),
        0,
        "closing the episode on the drain must reset the shared counter"
    );
    assert_eq!(
        dropped.unnamed(),
        0,
        "the drain is the last chance to name a gap; nothing may be left \
         outstanding on the ledger once the writer has acknowledged"
    );
}
