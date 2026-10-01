//! Shared deterministic async-queue fixtures; no production state is duplicated.

use crate::diag::DropLedger;
use crate::diag_shutdown_gate::ShutdownGate;
use std::sync::atomic::{AtomicUsize, Ordering};

/// What [`flood_a_stalled_async_queue`] observed, so the regression
/// test below is nothing but behavioural assertions.
pub(super) struct StalledQueueRun {
    /// How long the flood itself took. The enqueue half must never
    /// block, so this is the "did it park the LL-hook callback" probe.
    pub(super) flood_elapsed: std::time::Duration,
    /// Records the queue shed, read while NOTHING is consuming.
    pub(super) shed: u64,
    /// Records the queue accepted, likewise read with no consumer.
    pub(super) accepted: usize,
    /// Every line the writer handed to its sink, in order.
    pub(super) recorded: Vec<String>,
    /// `pending` after the writer drained — the `flush_async_for_tests`
    /// contract.
    pub(super) pending_after: usize,
    /// `dropped` after the writer drained — the counter must reset so
    /// the next burst reports its own size, not a running total.
    pub(super) dropped_after: u64,
    /// Drops the ledger still considers UNNAMED after the writer drained.
    /// Non-zero would mean a gap the log never told anyone about.
    pub(super) unnamed_after: u64,
}

/// Drive `enqueue_async_into` / `run_async_writer_loop` — the exact
/// halves `enqueue_async` / `ensure_async_writer` wire to the
/// process-wide statics — against a tiny channel, so the "queue filled
/// while the sink could not keep up" path is reachable at all: the
/// production queue is 256 deep and its sink is a file write no test
/// can pause.
///
/// ## Why this is two sequential phases and not one concurrent one
///
/// The original shape ran the writer on a scoped thread whose sink
/// parked on a condvar, flooded the channel "while the sink was
/// stalled", and then read `dropped`. That was racy and failed roughly
/// one run in six (CI run 30379264960; reproduced locally at 2/20 in
/// isolation, far worse under a loaded full-suite run): the writer
/// thread is free to `recv` its first record at any point DURING the
/// flood, and draining a record takes the outstanding count with it, so
/// the post-flood `dropped.load()` only counted the drops that happened
/// afterwards — observed 123 and 133 where the test demanded >= 199.
///
/// Production is *correct* there; the taken drops are not lost, they
/// are carried into the marker the writer emits. It was the test's
/// observation point that was racy — reading a counter a live writer is
/// entitled to take. So the two halves of the contract are observed at
/// points where nothing else can be running:
///
/// 1. **Accounting** — flood with NO consumer at all. `sync_channel`
///    buffers exactly `capacity` and `try_send` reports `Full` for
///    every one after that, so the split is exactly `capacity`
///    accepted / `overflow` shed, every run. This is also the strictest
///    possible version of "the enqueue must not block": with no
///    receiver ever running, a blocking `send` deadlocks instead of
///    merely being slow.
/// 2. **Reporting** — only then run the writer loop, on this thread,
///    over the already-closed channel. It drains, exits, and the sink
///    `Vec` is complete with no join to wait on. Running it inline also
///    removes the old hang hazard entirely: there is no parked thread
///    for a failing assertion to unwind into.
pub(super) fn flood_a_stalled_async_queue(capacity: usize, overflow: usize) -> StalledQueueRun {
    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(capacity);

    // ---- Phase 1: the queue ACCOUNTS for what it sheds. ----
    let flood_started = std::time::Instant::now();
    for i in 0..(capacity + overflow) {
        crate::diag::enqueue_async_into(&tx, &admission, &pending, &dropped, format!("flood #{i}"));
    }
    let flood_elapsed = flood_started.elapsed();
    let shed = dropped.unbound();
    let accepted = pending.load(Ordering::Relaxed);

    // ---- Phase 2: the writer REPORTS them as one coalesced marker. ----
    //
    // Close the channel first so the loop drains what survived and
    // returns; run it inline so there is no thread to join and no
    // ordering left to chance.
    drop(tx);
    let mut recorded: Vec<String> = Vec::new();
    crate::diag::run_async_writer_loop(rx, capacity, &pending, &dropped, |line| {
        recorded.push(line.to_owned());
    });

    StalledQueueRun {
        flood_elapsed,
        shed,
        accepted,
        recorded,
        pending_after: pending.load(Ordering::Relaxed),
        dropped_after: dropped.unbound(),
        unnamed_after: dropped.unnamed(),
    }
}

/// Lock-step handshake budget. Orders of magnitude above the microseconds
/// a handshake actually costs: it exists ONLY so a harness bug surfaces
/// as an assertable flag instead of hanging CI forever.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// What [`drive_a_saturated_async_queue`] observed. Every field is read
/// after all threads have joined, so no assertion can unwind into a
/// parked writer.
pub(super) struct SaturatedRun {
    /// Every line the writer handed to its sink, in order.
    pub(super) recorded: Vec<String>,
    /// `pending` after the pipeline drained.
    pub(super) pending_after: usize,
    /// Set when a lock-step handshake timed out. A harness failure, not a
    /// behavioural one — asserted separately so the two never get
    /// confused.
    pub(super) handshake_failed: bool,
}

/// Hold a real producer thread and the real writer loop in a SATURATED
/// steady state, deterministically.
///
/// ## Why this needs to be concurrent, and how it stays deterministic
///
/// The amplification only exists when records are shed WHILE the writer
/// is writing, so a harness that floods first and drains afterwards
/// cannot see it: with no producer running, every dequeued record but
/// the first carries a zero count. A free-running producer thread would
/// show it, but a previous free-running test on this branch failed ~1 run
/// in 6 (CI run 30379264960) — a probabilistic test that blocks a stack
/// is worse than no test.
///
/// So the two threads are real but LOCK-STEP. The writer's sink is the
/// seam: it runs on the writer thread, immediately after a record was
/// dequeued and therefore exactly while "the previous marker and record
/// are being written". It hands the producer a token, the producer
/// enqueues one record (which fits the slot the dequeue just freed) plus
/// `shed_per_round` more (which cannot fit, and are shed), then hands the
/// token back. Nothing races: the queue depth, the accept/shed split and
/// the carried count are the same on every run.
///
/// The queue starts full — `sync_channel` buffers exactly `capacity` and
/// nothing consumes during the prefill — so the steady state is reached
/// on the very first dequeue rather than being waited for.
pub(super) fn drive_a_saturated_async_queue(
    capacity: usize,
    rounds: usize,
    shed_per_round: usize,
) -> SaturatedRun {
    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let handshake_failed = std::sync::atomic::AtomicBool::new(false);
    let recorded = std::sync::Mutex::new(Vec::<String>::new());

    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(capacity);
    // Rendezvous pair: `go` is the writer saying "a slot just came free",
    // `done` is the producer saying "I have refilled it and shed the
    // rest". Unbounded channels so neither send can block.
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    for i in 0..capacity {
        crate::diag::enqueue_async_into(&tx, &admission, &pending, &dropped, format!("seed #{i}"));
    }

    let producer_tx = tx.clone();
    let admission_ref = &admission;
    let pending_ref = &pending;
    let dropped_ref = &dropped;
    let recorded_ref = &recorded;
    let failed_ref = &handshake_failed;

    std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            for round in 0..rounds {
                if go_rx.recv_timeout(HANDSHAKE_TIMEOUT).is_err() {
                    break;
                }
                // Exactly one fits the freed slot...
                crate::diag::enqueue_async_into(
                    &producer_tx,
                    admission_ref,
                    pending_ref,
                    dropped_ref,
                    format!("burst #{round}"),
                );
                // ...and everything after it arrives against a full queue.
                for i in 0..shed_per_round {
                    crate::diag::enqueue_async_into(
                        &producer_tx,
                        admission_ref,
                        pending_ref,
                        dropped_ref,
                        format!("shed #{round}.{i}"),
                    );
                }
                if done_tx.send(()).is_err() {
                    break;
                }
            }
            // `producer_tx` drops here, so once the writer has also seen
            // the outer `tx` go away it can disconnect and return.
        });

        scope.spawn(move || {
            let mut handshakes = 0usize;
            crate::diag::run_async_writer_loop(rx, capacity, pending_ref, dropped_ref, |line| {
                recorded_ref
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(line.to_owned());
                // Markers are not records: driving the producer from one
                // would change the accept/shed split depending on how
                // many markers the implementation chose to write, which
                // is precisely the variable under test.
                if line.starts_with("[diag-async]") || handshakes >= rounds {
                    return;
                }
                handshakes += 1;
                if go_tx.send(()).is_err() || done_rx.recv_timeout(HANDSHAKE_TIMEOUT).is_err() {
                    failed_ref.store(true, Ordering::Relaxed);
                }
            });
        });

        // The producer stops after exactly `rounds` handshakes; dropping
        // the last sender then lets the writer drain the tail, report it
        // and return, so the scope joins instead of hanging.
        let _ = producer.join();
        drop(tx);
    });

    let lines = recorded
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    SaturatedRun {
        recorded: lines,
        pending_after: pending.load(Ordering::Relaxed),
        handshake_failed: handshake_failed.load(Ordering::Relaxed),
    }
}

/// Pull the `dropped=<n>` count out of every marker line, in order.
///
/// Deliberately shape-agnostic (it matches both the episode-start and the
/// episode-summary marker) so the test states the CONTRACT — how many
/// markers, and does the last one name the whole burst — rather than
/// re-encoding one implementation's wording. The exact wording is pinned
/// by [`an_overload_burst_is_summarised_once_when_the_queue_catches_up`].
pub(super) fn reported_drop_counts(recorded: &[String]) -> Vec<u64> {
    recorded
        .iter()
        .filter(|line| line.starts_with("[diag-async]"))
        .map(|line| {
            let tail = line
                .split("dropped=")
                .nth(1)
                .unwrap_or_else(|| panic!("marker without a `dropped=` count: {line}"));
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("marker with an unparseable count: {line}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn stalled_fixture_drives_the_real_writer_with_exact_loss_accounting() {
        let run = super::flood_a_stalled_async_queue(4, 3);
        assert_eq!(run.accepted, 4);
        assert_eq!(run.shed, 3);
        assert_eq!(run.pending_after, 0);
        assert_eq!(run.unnamed_after, 0);
        assert_eq!(super::reported_drop_counts(&run.recorded), vec![3]);
    }
}
