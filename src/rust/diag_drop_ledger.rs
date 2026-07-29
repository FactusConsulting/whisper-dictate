//! Drop accounting for the off-callback diagnostic queue
//! ([`crate::diag`]'s `ASYNC_QUEUE_TX`).
//!
//! Extracted from `diag.rs` so the sink module stays readable and this
//! ledger - which is pure, lock-free logic and the single trickiest piece
//! of the queue - can be reasoned about and unit-tested on its own.
//! Re-exported as `crate::diag::DropLedger`, so every existing call site
//! and test path is unchanged.

use std::sync::atomic::{AtomicU64, Ordering};

/// Drop accounting for the off-callback queue.
///
/// Before this existed, a full queue dropped the line and told nobody:
/// `gui-diagnostic.log` looked identical whether the LL-hook callback
/// was quiet or whether the queue had shed a burst, so a reader could
/// not distinguish "nothing happened" from "the sink was too slow to
/// record what happened" — on exactly the slow-AppData scenario the
/// queue exists for.
///
/// ## Why TWO counters and not one
///
/// Codex P2 #682 comment 3669770197. A single counter conflates two
/// different questions, and the difference only becomes visible at
/// teardown:
///
/// * [`Self::unbound`] answers *"what should the NEXT accepted record
///   carry?"*. It has to be emptied by the producer (see
///   [`crate::diag::enqueue_async_into_after`]) so the marker lands at the
///   QUEUE POSITION of the gap rather than ahead of the older records
///   still queued.
/// * [`Self::unnamed`] answers *"what has no marker named yet?"*, and
///   that is the question the writer's shutdown arm must ask. With only
///   the first counter, a live rdev / raw-hook callback racing teardown
///   can empty it (its `take` runs BEFORE the drain queues its sentinel)
///   and then enqueue its record AFTER the sentinel. The shutdown arm
///   then reads zero, acknowledges, and `main` is free to exit before the
///   post-ack sweep runs — so a gap that happened BEFORE teardown, plus
///   the trace line that resumed after it, disappears despite the drain.
///
/// Splitting them removes the race outright rather than narrowing it:
/// `unnamed` is bumped at SHED time and decremented ONLY by the writer,
/// and only where a marker actually names the count. It is therefore
/// oblivious to where the count physically sits — on `unbound`, in a
/// producer's hand between the take and the send, or riding on a record
/// still in the channel. Nothing has to be waited for and no ordering has
/// to be guessed at.
///
/// Invariant: `unbound <= unnamed` at every point a writer can observe
/// them, which is why [`Self::shed`] bumps `unnamed` first.
pub(crate) struct DropLedger {
    /// Shed records whose count has not yet been bound to an accepted
    /// record. Taken to zero by the next ACCEPTED record — which carries
    /// the count to the writer so the marker keeps its queue position,
    /// see [`crate::diag::enqueue_async_into_after`] — or, when no such
    /// record ever arrives, by the writer itself on its park wakeup.
    unbound: AtomicU64,
    /// Shed records no marker has named yet, wherever their count
    /// currently sits. Decremented only by the writer, and only where a
    /// marker names the count.
    unnamed: AtomicU64,
}

impl DropLedger {
    pub(crate) const fn new() -> Self {
        Self {
            unbound: AtomicU64::new(0),
            unnamed: AtomicU64::new(0),
        }
    }

    /// Account for one shed record, handing `returned` — the count this
    /// producer had already taken and now has no record to ride on —
    /// back to [`Self::unbound`].
    ///
    /// `unnamed` is bumped BEFORE `unbound` so a writer that swaps
    /// `unbound` can never take a count `unnamed` does not already cover
    /// (which would underflow [`Self::mark_named`]). `returned` is
    /// deliberately NOT re-added to `unnamed`: it never left it.
    ///
    /// Two relaxed `fetch_add`s on the LL-hook thread; no allocation, no
    /// lock.
    pub(crate) fn shed(&self, returned: u64) {
        self.unnamed.fetch_add(1, Ordering::Relaxed);
        self.unbound.fetch_add(returned + 1, Ordering::Relaxed);
    }

    /// Take the outstanding count so it can be bound to a record.
    ///
    /// The `load` fast path matters: this runs on the Windows LL-hook
    /// callback thread for EVERY trace line, and the counter is zero in
    /// all but the handful of enqueues that follow an actual gap.
    pub(crate) fn take_unbound(&self) -> u64 {
        if self.unbound.load(Ordering::Relaxed) == 0 {
            0
        } else {
            self.unbound.swap(0, Ordering::Relaxed)
        }
    }

    /// Give up on `count` reserved drops that can never be reported —
    /// the writer is gone, so no marker will ever be emitted and leaving
    /// them on the ledger would only make it grow forever.
    pub(crate) fn forget(&self, count: u64) {
        self.mark_named(count);
    }

    /// Record that a marker has named `count` drops. Saturating, because
    /// [`Self::claim_every_unnamed`] can legitimately have named a count
    /// wholesale that a record still in the channel also carries.
    pub(crate) fn mark_named(&self, count: u64) {
        if count == 0 {
            return;
        }
        let _ = self
            .unnamed
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(count))
            });
    }

    /// Take the count no record has been given yet, for a marker that is
    /// about to name it. The ordinary close: it says nothing about counts
    /// already riding on queued records, because those records will
    /// arrive and name their own.
    pub(crate) fn claim_unbound(&self) -> u64 {
        let taken = self.unbound.swap(0, Ordering::Relaxed);
        self.mark_named(taken);
        taken
    }

    /// Take EVERYTHING still unnamed, wherever it sits. The teardown
    /// close: after this there is no "later" for a queued record to
    /// report itself in, so the last marker before the drain is
    /// acknowledged has to cover the whole outstanding gap.
    ///
    /// `unbound` is cleared first so a producer racing this cannot bind a
    /// count that was just named to a fresh record.
    pub(crate) fn claim_every_unnamed(&self) -> u64 {
        self.unbound.store(0, Ordering::Relaxed);
        self.unnamed.swap(0, Ordering::Relaxed)
    }

    /// Drops not yet bound to a record. Test-only observation point.
    #[cfg(test)]
    pub(crate) fn unbound(&self) -> u64 {
        self.unbound.load(Ordering::Relaxed)
    }

    /// Drops no marker has named yet. Test-only observation point.
    #[cfg(test)]
    pub(crate) fn unnamed(&self) -> u64 {
        self.unnamed.load(Ordering::Relaxed)
    }

    /// Test-only: put the ledger in the state `count` sheds with no
    /// record to carry them would have left it in.
    ///
    /// A shed REQUIRES a full queue and a live writer drains the queue,
    /// so "a drop lands while the writer is parked on an empty queue"
    /// cannot be scheduled deterministically from the producer side.
    #[cfg(test)]
    pub(crate) fn shed_for_tests(&self, count: u64) {
        self.unnamed.fetch_add(count, Ordering::Relaxed);
        self.unbound.fetch_add(count, Ordering::Relaxed);
    }

    /// Test-only: state that `count` drops are outstanding but already
    /// bound to records the test hand-built rather than produced through
    /// [`crate::diag::enqueue_async_into`].
    #[cfg(test)]
    pub(crate) fn carried_for_tests(&self, count: u64) {
        self.unnamed.fetch_add(count, Ordering::Relaxed);
    }
}
