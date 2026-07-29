//! Admission gate for the off-callback diagnostic queue
//! ([`crate::diag`]'s `ASYNC_QUEUE_TX`).
//!
//! ## The starvation this exists to stop (Codex P2 #681 comment 3669689764)
//!
//! [`crate::diag::drain_and_shutdown_into`] hands its shutdown sentinel to
//! the SAME bounded queue as the trace records, because FIFO ordering is
//! what makes "everything queued before the drain request is already
//! written" true. A bounded queue can be full, so the sentinel is offered
//! with `try_send` and re-offered after a short nap until it fits or the
//! deadline runs out.
//!
//! That nap is the hole. The producer is the Windows `WH_KEYBOARD_LL` /
//! rdev callback thread, which is unjoinable and keeps firing all through
//! teardown - under the documented `VOICEPI_LOG=debug` mouse trace it
//! offers a record roughly every millisecond. Against a slow-but-perfectly
//! functional sink, every slot the writer frees is handed straight back to
//! that producer long before the teardown thread wakes up to retry. The
//! sentinel can therefore be starved for the WHOLE deadline and the
//! process exits with the pre-request backlog undrained - while the writer
//! was never wedged at all, so the operator gets a "the tee file may be
//! short of records" warning that blames the wrong thing.
//!
//! Polling harder does not fix it: the producer wins any race it enters,
//! because it is already running on a hot callback path and the teardown
//! thread has to be scheduled first. The only shape that works is to stop
//! the producer, which is what this gate does - [`ShutdownGate::close`]
//! runs BEFORE the first `try_send` attempt, and every subsequent
//! [`crate::diag::enqueue_async`] is refused without touching the channel.
//! From that instant the queue is drained by the writer and refilled by
//! nobody, so the first slot the writer frees is the sentinel's.
//!
//! ## Why refused lines are not counted as shed records
//!
//! Everything the gate refuses is, by construction, YOUNGER than the drain
//! request: the gate closes inside `drain_and_shutdown_into`, so any line
//! that meets a closed gate was produced after teardown began. The queue's
//! contract already says such traffic is not what the drain guarantees -
//! `drain_and_ack_shutdown` acknowledges first and sweeps post-sentinel
//! records only on borrowed time, discarding the rest without a marker.
//!
//! Counting them onto [`crate::diag::DropLedger`] would also be an
//! accounting the ledger cannot honour. Refusals continue after the writer
//! thread has returned, so any number a marker could name is a snapshot
//! taken mid-stream, and this ledger's stated discipline is that the
//! accounting may be late but never wrong. A knowingly partial count is
//! worse than none: it reads as the size of the gap and is not.
//!
//! The gate deliberately does NOT touch the reservation a producer may be
//! holding either - [`crate::diag::enqueue_async_into_after`] checks the
//! gate BEFORE `DropLedger::take_unbound`, so a genuine pre-teardown gap
//! stays on the ledger and is named by the shutdown close exactly as it
//! was before this gate existed.

use std::sync::atomic::{AtomicBool, Ordering};

/// One-way latch controlling whether the off-callback diagnostic queue
/// still accepts new trace lines.
///
/// Open for the whole life of the process until teardown closes it; there
/// is deliberately no `reopen` outside tests, because the only caller that
/// closes it is a `main` on its way out and a queue that started accepting
/// lines again would hand the starvation straight back.
pub(crate) struct ShutdownGate {
    /// `false` while the queue accepts records. Never returns to `false`
    /// in a shipping process.
    closed: AtomicBool,
}

impl ShutdownGate {
    /// A gate that admits everything. `const` so the process-wide instance
    /// can be a plain `static` rather than a `OnceLock`, which keeps
    /// [`Self::admits`] a single relaxed load on the LL-hook hot path.
    pub(crate) const fn new() -> Self {
        Self {
            closed: AtomicBool::new(false),
        }
    }

    /// True while new trace lines may be offered to the queue.
    ///
    /// `Relaxed` is the right ordering and not a shortcut: the flag
    /// publishes no data alongside itself, so there is nothing for an
    /// `Acquire` to order against, and correctness only needs producers to
    /// see the close *eventually* - a producer that squeezes one last
    /// record in costs the teardown one extra poll, not the deadline.
    /// This runs on the Windows `WH_KEYBOARD_LL` callback thread for every
    /// trace line, where a single relaxed load is the whole budget.
    #[inline]
    pub(crate) fn admits(&self) -> bool {
        !self.closed.load(Ordering::Relaxed)
    }

    /// Stop admitting new lines. Idempotent - teardown can run twice on a
    /// nested error path, and a second close must be a no-op rather than
    /// an error.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }

    /// Test-only observation point: the companion tests assert that the
    /// drain closes the gate rather than inferring it from a timing.
    #[cfg(test)]
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}
