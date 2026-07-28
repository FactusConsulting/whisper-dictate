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

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::diag_async::{enqueue, spawn_writer_with_sink};

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
/// the queue that replaces it is correct: an unbounded MPSC with a
/// single-consumer writer thread whose ordering and completeness
/// match the send order exactly. A regression that (say) swapped in
/// a bounded channel with `try_send` and started silently dropping
/// records would fail here.
#[test]
fn queue_receives_and_writes_records_asynchronously() {
    let sink: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_writer = Arc::clone(&sink);
    let tx = spawn_writer_with_sink(move |msg| {
        sink_writer.lock().unwrap().push(msg.to_owned());
    });

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
    });
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
    let (dead_tx, dead_rx) = std::sync::mpsc::channel::<String>();
    drop(dead_rx);
    enqueue(&dead_tx, "would-panic-if-unwrapped".to_owned());
    // If we got here, enqueue did not panic. That is the property.
}
