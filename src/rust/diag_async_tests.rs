//! Companion tests for [`crate::diag_async`]. Extracted from an inline
//! `#[cfg(test)] mod tests` in `diag_async.rs` so the regression-test
//! discipline scanner (per AGENTS.md,
//! `enforce-regression-test-discipline`) sees a matching test file next
//! to the production module.
//!
//! Tests here cover the async pipeline's ordering / completeness /
//! silent-drop-on-dead-consumer semantics — the properties the LL-hook
//! callback relies on for its "diagnostic write MUST NOT stall the
//! hook thread" contract (Codex P2 #651 discussion
//! PRRT_kwDOSfNjQs6UTvPm).

#![cfg(test)]

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::diag_async::{
    dropped_marker, enqueue, spawn_writer_with_capacity, spawn_writer_with_sink, WriterHandle,
    WriterMessage, QUEUE_CAPACITY,
};

/// Drain `sink` until it has received at least `expected` records or
/// `timeout` elapses. Shared helper so every test uses the same
/// polling discipline (short poll interval, bounded wait, no busy
/// spin). Returns the current record count either way — the caller
/// asserts on it.
fn wait_for_records(sink: &Arc<Mutex<Vec<String>>>, expected: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        let count = sink.lock().unwrap().len();
        if count >= expected {
            return count;
        }
        if Instant::now() >= deadline {
            return count;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Load-bearing regression test: the async writer must receive every
/// enqueued record and forward it to the sink in order. Without this
/// pipeline, the LL-hook callback synchronously acquires the diag
/// writer mutex + flushes the tee file on every keyboard event, which
/// on a slow AppData volume can exceed Windows' ~300 ms LL-hook
/// timeout and cause the OS to silently uninstall the PTT hook
/// (Codex P2 #651 discussion PRRT_kwDOSfNjQs6UTvPm).
///
/// The un-fixed code path is the direct `crate::diag::log!` call
/// inside the rdev callback — it never goes through this queue, so
/// this test cannot itself observe the un-fixed failure mode
/// (a mutex acquisition in the callback). What it CAN pin is that
/// the queue that replaces it is correct: a single-consumer writer
/// thread whose ordering and completeness match the send order
/// exactly, for any burst that fits inside [`QUEUE_CAPACITY`]. A
/// regression that started shedding records BELOW the bound would
/// fail here; shedding ABOVE the bound is the documented, marker-
/// accounted behaviour pinned by
/// `bounded_queue_sheds_and_reports_a_coalesced_dropped_marker`.
#[test]
fn queue_receives_and_writes_records_asynchronously() {
    let sink: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_writer = Arc::clone(&sink);
    let tx = spawn_writer_with_sink(move |msg| {
        sink_writer.lock().unwrap().push(msg.to_owned());
    })
    .expect("writer thread must spawn on a healthy host");

    // Push N records in order. `enqueue` returns immediately (no
    // blocking, no mutex acquisition on the sender side) — that's
    // the property the LL-hook callback needs.
    let n = 25usize;
    for i in 0..n {
        enqueue(&tx, format!("record #{i}"));
    }

    // Drop the sender so the writer thread's `recv()` loop exits
    // cleanly after draining — otherwise the polling helper would
    // spin until timeout even after all records landed.
    drop(tx);
    let observed = wait_for_records(&sink, n, Duration::from_secs(2));
    assert_eq!(
        observed, n,
        "async writer must forward every enqueued record to the sink; \
         got {observed} of {n}"
    );

    // Ordering — the writer is a single-consumer loop so records
    // land in send order. A regression that used a multi-consumer
    // work-stealing pool would fail this assertion; keep it strict.
    let recorded = sink.lock().unwrap().clone();
    for (i, line) in recorded.iter().enumerate() {
        assert_eq!(
            line,
            &format!("record #{i}"),
            "records must be forwarded in send order; index {i} got {line}"
        );
    }
}

/// Enqueue MUST NOT panic when the writer thread has stopped (its
/// sender was dropped by the test host / a future rework). The
/// LL-hook callback calls `enqueue` unconditionally on every event
/// — a panic here would take the LL-hook thread down and the whole
/// PTT subsystem with it. Codex P2 #651 discussion
/// PRRT_kwDOSfNjQs6UTvPm.
#[test]
fn enqueue_silently_drops_when_writer_thread_is_gone() {
    let sink: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_writer = Arc::clone(&sink);
    let tx = spawn_writer_with_sink(move |msg| {
        sink_writer.lock().unwrap().push(msg.to_owned());
    })
    .expect("writer thread must spawn on a healthy host");
    // Send one record, then close the pipeline by dropping the
    // sender. The writer thread's recv loop exits, giving us the
    // "consumer gone" shape.
    enqueue(&tx, "first".to_owned());
    let _ = wait_for_records(&sink, 1, Duration::from_secs(2));
    drop(tx);
    // Give the writer thread a moment to notice the disconnect.
    std::thread::sleep(Duration::from_millis(20));

    // Now build a fresh channel whose receiver is dropped
    // immediately — enqueue against it must silently return.
    let (dead_tx, dead_rx) = mpsc::sync_channel::<WriterMessage>(4);
    drop(dead_rx);
    let dead = WriterHandle::from_sender_for_tests(dead_tx);
    enqueue(&dead, "would-panic-if-unwrapped".to_owned());
    // If we got here, enqueue did not panic. That is the property.
    // A disconnected channel is NOT counted as a shed record: there is
    // no writer left to ever emit the marker, so counting would grow a
    // number nobody reads.
    assert_eq!(
        dead.dropped_count(),
        0,
        "a disconnected writer must not accumulate drop counts — only a \
         FULL queue does, because only then can the writer still emit the \
         `dropped=` marker"
    );
}

/// Codex P2 #675 PRRT_kwDOSfNjQs6UbAiW — `drain_and_shutdown` must
/// forward every queued record to the sink before releasing the
/// waiter. Without this, a `main` that returns while the diag
/// writer has a backlog would kill the writer thread mid-drain and
/// lose whatever was still in the channel. The static
/// [`crate::diag_async::WRITER`] slot cannot be reset from tests
/// (`OnceLock` has no take), so we build a synthetic writer on the
/// side that reuses the same `WriterMessage` protocol and asserts
/// the drain-then-ack contract end-to-end.
///
/// FAILS on the pre-fix design: the writer had no shutdown sentinel
/// at all; the only way to end the thread was to drop the sender,
/// which the production static slot never does. A process exit
/// therefore terminated the thread with any pending Line records
/// still in the queue.
#[test]
fn drain_and_shutdown_flushes_pending_records_before_ack() {
    let sink: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_writer = Arc::clone(&sink);
    let tx = spawn_writer_with_sink(move |msg| {
        // Sleep a bit per record so the shutdown sentinel enters the
        // queue while the writer is still processing earlier records
        // — the exact condition drain_and_shutdown must handle.
        std::thread::sleep(Duration::from_millis(5));
        sink_writer.lock().unwrap().push(msg.to_owned());
    })
    .expect("writer thread must spawn on a healthy host");

    let n = 20usize;
    for i in 0..n {
        enqueue(&tx, format!("pending #{i}"));
    }

    // Emulate `drain_and_shutdown` against the local sender.
    let (ack_tx, ack_rx) = mpsc::channel::<()>();
    assert!(
        tx.send_shutdown_for_tests(ack_tx),
        "shutdown must reach writer"
    );

    let acked = ack_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    assert!(
        acked,
        "writer must ack Shutdown within the deadline so the caller \
         (GUI main) does not block on process exit"
    );

    // ACK arrives only after the writer drains every pending Line —
    // so every record must be in the sink by now, no additional
    // polling required.
    let recorded = sink.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        n,
        "drain_and_shutdown must flush all pending records before ack; \
         got {} of {}",
        recorded.len(),
        n
    );
    for (i, line) in recorded.iter().enumerate() {
        assert_eq!(line, &format!("pending #{i}"));
    }
}

// -----------------------------------------------------------------------
// Codex P2 #675 PRRT_kwDOSfNjQs6Uc5ki — bound the diagnostic queue
// under slow sinks, and account for what gets shed.
// -----------------------------------------------------------------------

/// The queue MUST NOT grow without limit when the sink stalls.
///
/// Un-fixed shape: `mpsc::channel()` (unbounded) + `Sender::send`. With
/// a stalled sink every `enqueue` still succeeded, so the channel
/// retained one formatted `String` per record forever. At
/// `VOICEPI_LOG=trace` the rdev callback enqueues its `raw=` record
/// BEFORE the non-key filter runs, so `MouseMove` at the pointer's
/// report rate feeds this queue — sustained mouse movement against a
/// wedged AppData volume grows the GUI's memory until it is unstable
/// or OOM-killed.
///
/// Fixed shape: a bounded `sync_channel` + non-blocking `try_send`.
/// Records past the bound are shed, and every shed record is counted
/// and reported to the sink as ONE coalesced [`dropped_marker`] line
/// so a reader of the tee file knows the trace has a gap.
///
/// The test uses [`spawn_writer_with_capacity`] with a tiny bound so
/// the arithmetic is exact and the run is fast; the production bound
/// is pinned separately by `queue_capacity_is_a_finite_bound`.
#[test]
fn bounded_queue_sheds_and_reports_a_coalesced_dropped_marker() {
    const CAPACITY: usize = 8;
    const OVERFLOW: usize = 200;

    // A sink that parks on `gate` for its FIRST record, standing in for
    // a wedged AppData write. Everything after the release runs fast.
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let gate_sink = Arc::clone(&gate);
    let sink: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_writer = Arc::clone(&sink);
    let mut stalled_once = false;
    let tx = spawn_writer_with_capacity(CAPACITY, move |msg| {
        if !stalled_once {
            stalled_once = true;
            let (lock, cv) = &*gate_sink;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cv.wait(released).unwrap();
            }
        }
        sink_writer.lock().unwrap().push(msg.to_owned());
    })
    .expect("writer thread must spawn on a healthy host");

    // Flood far past the bound while the sink is parked. `enqueue` must
    // return promptly for every single one of these — a blocking send
    // here would hang the LL-hook thread, which is the whole reason the
    // async writer exists.
    let flood_started = Instant::now();
    for i in 0..(CAPACITY + OVERFLOW) {
        enqueue(&tx, format!("flood #{i}"));
    }
    let flood_elapsed = flood_started.elapsed();
    assert!(
        flood_elapsed < Duration::from_secs(2),
        "enqueue must never block on a full queue (LL-hook budget is ~300ms \
         for the WHOLE callback); flooding {} records took {flood_elapsed:?}",
        CAPACITY + OVERFLOW
    );

    // The bound must have bitten. On the un-fixed unbounded channel this
    // is 0 — every record was accepted and retained.
    let shed = tx.dropped_count();
    assert!(
        shed > 0,
        "a bounded queue must shed records once the stalled sink lets it \
         fill; dropped_count()=0 means the channel is still unbounded and \
         the queue grows without limit (Codex P2 #675 PRRT_kwDOSfNjQs6Uc5ki)"
    );
    assert!(
        shed >= OVERFLOW as u64,
        "with capacity={CAPACITY} and {} records flooded, at least {OVERFLOW} \
         must be shed; got {shed}",
        CAPACITY + OVERFLOW
    );

    // Release the sink, then drain-and-shutdown so every surviving
    // record plus the accounting marker lands before we assert.
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    let (ack_tx, ack_rx) = mpsc::channel::<()>();
    assert!(
        tx.send_shutdown_for_tests(ack_tx),
        "shutdown must reach the writer"
    );
    assert!(
        ack_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
        "writer must ack the drain once the sink is released"
    );

    let recorded = sink.lock().unwrap().clone();
    // Memory bound, restated on the output side: the sink can never
    // have seen more than the records that fit plus markers.
    assert!(
        recorded.len() <= CAPACITY + 2,
        "the writer must only ever have held ~capacity records; saw {} lines",
        recorded.len()
    );

    // Accounting: exactly ONE coalesced marker, not one per drop.
    let markers: Vec<&String> = recorded
        .iter()
        .filter(|line| line.contains("[diag-async] dropped="))
        .collect();
    assert_eq!(
        markers.len(),
        1,
        "drops must be reported as ONE coalesced marker, never one line per \
         drop (that would be unbounded write amplification against the same \
         stalled sink); got {markers:?} out of {recorded:?}"
    );
    assert_eq!(
        markers[0],
        &dropped_marker(shed, CAPACITY),
        "the marker must name the exact shed count and the capacity that was \
         exceeded, so a log reader can size the gap"
    );
}

/// The production bound must be a real, finite number. A regression
/// that set it to `usize::MAX` (or reverted to an unbounded channel)
/// would reintroduce the unbounded-memory hazard while leaving the
/// shedding machinery cosmetically in place.
#[test]
fn queue_capacity_is_a_finite_bound() {
    assert!(
        QUEUE_CAPACITY > 0,
        "a zero capacity would make sync_channel a rendezvous channel and \
         shed nearly every record"
    );
    assert!(
        QUEUE_CAPACITY <= 65_536,
        "QUEUE_CAPACITY={QUEUE_CAPACITY} is large enough to be effectively \
         unbounded — the point of Codex P2 #675 PRRT_kwDOSfNjQs6Uc5ki is a \
         memory ceiling a stalled sink cannot blow through"
    );
}

/// The pre-fix `spawn_writer_with_sink` panicked (`.expect`) on
/// thread::Builder::spawn failure. The fix returns
/// `Result<Sender, String>` so the rdev listener can surface the
/// failure via `SpawnError::WriterStartup` instead of losing every
/// diagnostic record silently. Codex P2 #675 PRRT_kwDOSfNjQs6UbAip.
///
/// We can't force `thread::Builder::spawn` to fail in a unit test —
/// so this test locks the SUCCESSFUL contract (Ok on a healthy host)
/// plus the error surface (the Err type is `String` so the caller
/// can format it into a `SpawnError`). The `SpawnError::WriterStartup`
/// side is pinned by
/// `driver_common::tests::spawn_error_writer_startup_variant_carries_context_and_is_distinct`.
#[test]
fn spawn_writer_with_sink_returns_result_not_panic() {
    let outcome: Result<_, String> = spawn_writer_with_sink(|_msg| {});
    assert!(
        outcome.is_ok(),
        "on a healthy host the spawn must succeed and return Ok(Sender); \
         Codex P2 #675 PRRT_kwDOSfNjQs6UbAip pins the Result-not-panic \
         contract so a listener that primes the writer can surface a \
         hypothetical spawn failure via SpawnError::WriterStartup instead \
         of taking down the LL-hook thread"
    );
}
