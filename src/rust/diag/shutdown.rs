//! Bounded shutdown sentinel admission and acknowledgement.

use super::{AsyncRecord, ShutdownGate, ASYNC_GATE, ASYNC_QUEUE_TX};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

/// How long [`drain_and_shutdown`] naps between `try_send` attempts
/// while the bounded queue is full. Short enough that a queue draining
/// normally costs no measurable teardown latency, long enough that a
/// genuinely wedged writer is not spun on for the whole deadline.
const ASYNC_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Flush everything still queued for the off-callback writer and stop
/// the writer thread, waiting at most `deadline`.
///
/// ## Why this exists
///
/// The writer is a background thread. A `fn main` that simply returns
/// takes the process down with whatever is still in the queue, so the
/// records closest to the moment of interest - the tail of a PTT wedge
/// repro, the last chord trace before a crash-adjacent exit - are
/// exactly the ones a support thread never gets to read. Both shipping
/// binaries hit this: the GUI on tray exit, and the CLI on its finite
/// rdev-driven verbs (`self-test hotkey-boot`, `hotkey capture
/// --for-secs ...`) which install the same LL hook, feed the same
/// queue, and then return normally.
///
/// ## How
///
/// Pushes an [`AsyncRecord::Shutdown`] sentinel through the SAME
/// bounded queue as the records, so every record enqueued before the
/// call is ordered ahead of it; the writer flushes them, emits any
/// outstanding drop marker, acks and exits.
///
/// Returns `true` ONLY when the writer acknowledged within `deadline`,
/// or when no writer was ever installed (nothing was ever queued, so
/// nothing can be lost). Everything else is `false`: a timeout, and
/// since a receiver that had
/// already disappeared, which means the thread never started (
/// [`ensure_async_writer`] swallows spawn errors) or panicked, and took
/// the queue with it unacknowledged. The bool is what lets the caller
/// warn the operator that the tee file may be short of records;
/// production must NOT treat it as fatal.
pub fn drain_and_shutdown(deadline: Duration) -> bool {
    match ASYNC_QUEUE_TX.get() {
        Some(tx) => drain_and_shutdown_into(tx, &ASYNC_GATE, deadline),
        // Never installed: no writer thread, no queue, nothing lost.
        None => true,
    }
}

/// Sender half of [`drain_and_shutdown`], parameterised over the
/// channel and the gate so `diag_tests` can drive every outcome (clean
/// flush, wedged-past-deadline, saturated producer) against a scoped
/// writer instead of the process-wide `OnceLock`, which no test can
/// reset.
///
/// Delegates to [`drain_and_shutdown_into_after`] with an empty seam, so
/// the function the companion tests drive IS the production drain rather
/// than a parallel copy.
pub(crate) fn drain_and_shutdown_into(
    tx: &SyncSender<AsyncRecord>,
    gate: &ShutdownGate,
    deadline: Duration,
) -> bool {
    drain_and_shutdown_into_after(tx, gate, deadline, || {})
}

/// [`drain_and_shutdown_into`] with a seam that runs immediately before
/// each `try_send` attempt.
///
/// `before_attempt` is where `diag_tests` reproduces the exact
/// interleaving the test reproduces - the writer frees
/// one slot and the still-firing callback producer refills it - without a
/// sleep or a scheduling race. Production passes an empty closure.
///
/// ## The gate closes BEFORE the first attempt
///
/// The queue is BOUNDED, so a plain `send` here could park forever behind
/// a stalled writer with a full queue - exactly the teardown hang the
/// deadline exists to prevent. `SyncSender::send_timeout` is still
/// unstable, so poll `try_send` (which hands the message back on `Full`)
/// inside the same overall budget.
///
/// Polling alone is not enough, and that is what the gate fixes. Between
/// two attempts this thread sleeps [`ASYNC_DRAIN_POLL_INTERVAL`], while
/// the producer is an unjoinable OS-callback thread offering a record
/// roughly every millisecond under the documented `VOICEPI_LOG=debug`
/// mouse trace. Every slot a slow-but-functional writer frees therefore
/// goes back to the producer before this thread wakes up, and the
/// sentinel can be starved for the entire deadline - the process exits
/// with the pre-request backlog undrained and warns the operator about a
/// writer that was never wedged.
///
/// Closing the gate first makes the queue drain-only: from that point the
/// writer removes records and nobody adds any, so the first slot it frees
/// is necessarily the sentinel's. It is a one-way latch and teardown may
/// run twice on a nested error path, so the close is idempotent.
pub(crate) fn drain_and_shutdown_into_after<H>(
    tx: &SyncSender<AsyncRecord>,
    gate: &ShutdownGate,
    deadline: Duration,
    mut before_attempt: H,
) -> bool
where
    H: FnMut(),
{
    // Stop admitting new lines BEFORE polling for sentinel space.
    gate.close();
    let started = Instant::now();
    let (ack_tx, ack_rx) = mpsc::channel::<()>();
    let mut sentinel = AsyncRecord::Shutdown(ack_tx);
    loop {
        before_attempt();
        match tx.try_send(sentinel) {
            Ok(()) => break,
            // The receiver is gone and our sentinel never reached a
            // writer, so nothing acknowledged the drain and whatever
            // was queued died with the thread. Reachable in production:
            // `ensure_async_writer` deliberately swallows a thread-spawn
            // error, and a panicking writer drops its receiver the same
            // way. Reporting success here would suppress the exit
            // warning and contradict this function's documented
            // contract on exactly the runs where diagnostics WERE lost
            // The receiver can disappear after the writer thread exits.
            //
            // There is no "already acknowledged, then disconnected"
            // case to rescue: an ack can only arrive on the `ack_rx`
            // below, which is reached only after a sentinel was
            // accepted. Every `Disconnected` on this send is therefore
            // an unexpected one.
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(returned)) => {
                let remaining = deadline.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    // The queue stayed full for the whole budget: the
                    // writer is wedged and the caller (a `main` on its
                    // way out) must not wait any longer.
                    return false;
                }
                sentinel = returned;
                thread::sleep(ASYNC_DRAIN_POLL_INTERVAL.min(remaining));
            }
        }
    }
    let remaining = deadline.saturating_sub(started.elapsed());
    // `is_ok()` folds BOTH failure shapes into `false` on purpose: a
    // `Timeout` (the writer is wedged) and a `Disconnected` (the writer
    // dropped the ack sender without answering - it panicked mid-drain)
    // are equally "the tee file may be short of records", which is the
    // only thing the caller does with this bool.
    ack_rx.recv_timeout(remaining).is_ok()
}
