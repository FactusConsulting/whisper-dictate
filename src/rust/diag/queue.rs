//! Nonblocking callback producer using the single shared gate and ledger.

use super::{
    AsyncRecord, DropLedger, ShutdownGate, ASYNC_DROPPED, ASYNC_GATE, ASYNC_PENDING, ASYNC_QUEUE_TX,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};

/// Enqueue one trace line for the off-callback writer thread. Returns
/// silently when the writer has not been installed yet (early startup
/// racing the driver spawn) OR when the queue is full — dropping the
/// line is always preferable to blocking the LL-hook callback (see
/// [`ASYNC_QUEUE_CAPACITY`] for the rationale).
///
/// A drop is never silent to the LOG READER, though: it bumps
/// [`ASYNC_DROPPED`], and the writer thread announces the overload as
/// one [`async_dropped_marker`] line ahead of the first record accepted
/// after the gap, plus one [`async_burst_summary_marker`] naming the
/// episode total once the queue catches up (see [`BurstState`]). Without
/// that, a shed burst and a quiet period are indistinguishable in
/// `gui-diagnostic.log`, which makes the trace untrustworthy for exactly
/// the slow-sink scenario the queue exists for.
///
/// Callers on the LL-hook path MUST use this function (or the
/// [`log_async!`] macro) instead of [`log!`]. The two are otherwise
/// equivalent — the writer thread invokes the same `write_line` sink
/// internally, so the resulting `gui-diagnostic.log` line format is
/// unchanged.
pub fn enqueue_async(message: String) {
    if let Some(tx) = ASYNC_QUEUE_TX.get() {
        enqueue_async_into(tx, &ASYNC_GATE, &ASYNC_PENDING, &ASYNC_DROPPED, message);
    }
}

/// Sender half of [`enqueue_async`], parameterised over the channel and
/// the two counters so `diag_tests` can drive the shedding path against
/// a queue small enough to fill deterministically.
///
/// `try_send` (never `send`) is the whole point: this runs on the
/// Windows `WH_KEYBOARD_LL` callback thread, where the only acceptable
/// outcomes are "queued" and "shed" — never "parked". Blocking until
/// the writer caught up would defeat the entire purpose of the queue on
/// the exact "slow AppData volume" scenario it exists for.
///
/// ## Why the shed count travels WITH the accepted record
///
/// `Full` sheds the NEWEST record while up to `capacity` OLDER accepted
/// records are still queued ahead of it. If the writer instead read a
/// shared counter at write time, it would announce the gap before the
/// first record it happened to dequeue — records that were accepted
/// BEFORE the gap — so a reader correlating `t=<ms>` prefixes against
/// the moment PTT died would see the gap up to a whole backlog too
/// early. Handing the outstanding
/// count to the first record ACCEPTED after the gap pins the marker to
/// the right queue position instead.
///
/// The take is a `load`-then-`swap` so the healthy path (counter zero,
/// every enqueue) costs one relaxed load rather than a
/// read-modify-write on the LL-hook thread. A `Full` hands back what it
/// took plus its own record, so a count is never lost — the accounting
/// is allowed to be late, never wrong.
pub(crate) fn enqueue_async_into(
    tx: &SyncSender<AsyncRecord>,
    gate: &ShutdownGate,
    pending: &AtomicUsize,
    dropped: &DropLedger,
    message: String,
) {
    enqueue_async_into_after(tx, gate, pending, dropped, message, || {});
}

/// [`enqueue_async_into`] with a seam between the drop RESERVATION and
/// the send.
///
/// `reserved` runs after [`DropLedger::take_unbound`] has emptied the
/// unbound counter and before the record is offered to the channel
/// which is exactly the window in which a concurrent
/// [`drain_and_shutdown_into`] can slip its sentinel into the queue
/// ahead of this record. Production
/// passes an empty closure, so the two functions are literally the same
/// code path; `diag_tests` passes the sentinel enqueue, which is the only
/// way to reproduce that interleaving without a sleep and a prayer.
///
/// ## The gate check comes FIRST
///
/// Once teardown has closed `gate`, this refuses the line without
/// touching the channel at all. That is what keeps a producer which is
/// still firing through teardown - the unjoinable rdev / raw-hook
/// callback thread - from taking back every slot the writer frees and
/// starving the shutdown sentinel for the whole exit deadline; see
/// [`crate::diag_shutdown_gate`].
///
/// The check runs BEFORE [`DropLedger::take_unbound`] on purpose: a
/// refused line must not disturb a gap the ledger is still holding, so a
/// genuine pre-teardown overload is named by the shutdown close exactly
/// as it was before the gate existed. The refusal itself is deliberately
/// not counted - see the module docs for why a partial count is worse
/// than none here.
pub(crate) fn enqueue_async_into_after<H>(
    tx: &SyncSender<AsyncRecord>,
    gate: &ShutdownGate,
    pending: &AtomicUsize,
    dropped: &DropLedger,
    message: String,
    reserved: H,
) where
    H: FnOnce(),
{
    if !gate.admits() {
        return;
    }
    let drops_before = dropped.take_unbound();
    reserved();
    match tx.try_send(AsyncRecord::Line {
        drops_before,
        message,
    }) {
        Ok(()) => {
            pending.fetch_add(1, Ordering::Relaxed);
        }
        Err(TrySendError::Full(_)) => {
            // Shed the newest record but ACCOUNT for it, so the writer
            // can tell the log reader the trace has a gap and how big.
            // The count we took has no record to ride on after all, so
            // it goes back on the counter along with this drop.
            dropped.shed(drops_before);
        }
        Err(TrySendError::Disconnected(_)) => {
            // The writer thread is gone. Not counted (and what we took
            // is deliberately not restored): the marker could never be
            // emitted, so the counter would only grow forever.
            dropped.forget(drops_before);
        }
    }
}
