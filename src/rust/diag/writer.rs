//! Async queue consumer and bounded overload-episode accounting.

use super::{AsyncRecord, DropLedger, ASYNC_PARK_POLL};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};

/// The marker the writer emits ONCE, at the moment an overload episode
/// starts, immediately ahead of the first record accepted after the gap.
///
/// A named function rather than an inline `format!` so the regression
/// test can assert the exact shape without duplicating the format
/// string, and so a future log-parsing tool has one place to match.
///
/// ASCII only: this string reaches stderr via [`write_line`] and
/// typographic punctuation renders as mojibake under cmd.exe on a
/// legacy code page (AGENTS.md; pinned by `console_ascii_tests`).
pub(crate) fn async_dropped_marker(dropped: u64, capacity: usize) -> String {
    format!(
        "[diag-async] dropped={dropped} record(s): the diagnostic queue \
         (capacity={capacity}) filled while the sink was slow - the trace \
         below has a gap"
    )
}

/// The marker that CLOSES an overload episode: one line naming the whole
/// episode's shed total, emitted when the queue has demonstrably caught
/// up (see [`BurstState`]).
///
/// Deliberately reports the episode TOTAL rather than "the part not yet
/// named above": a reader scanning for the size of the gap should be
/// able to read one number off one line instead of summing a
/// start-marker and a remainder, and the wording says `in total` so the
/// overlap with the start marker cannot be misread as two gaps.
pub(crate) fn async_burst_summary_marker(dropped: u64, capacity: usize) -> String {
    format!(
        "[diag-async] overload burst ended: dropped={dropped} record(s) in \
         total while the diagnostic queue (capacity={capacity}) stayed \
         full - the trace is complete again from here on"
    )
}

/// How many consecutive ACCEPTED records with nothing shed ahead of them
/// end an overload episode.
///
/// The producer and the writer are back in balance once the queue has
/// room at every enqueue, so a run of clean records is the "caught up"
/// signal. It is a RUN rather than a single record because a queue
/// hovering exactly at its bound alternates accept / shed, and ending
/// the episode on the first clean record would restart it on the next
/// shed — reintroducing the per-record amplification this exists to
/// stop. 16 is small enough that the summary lands promptly in a
/// resumed trace (16 records is a fraction of a second of the debug-level
/// mouse stream) and large enough that ordinary jitter around the bound
/// cannot flap it.
///
/// Not the only exit: an empty queue observed at an [`ASYNC_PARK_POLL`]
/// wakeup also closes the episode, which is what covers a burst that
/// ends the trace outright.
pub(crate) const ASYNC_BURST_CLEAR_RUN: usize = 16;

/// Writer-side state for one overload episode.
///
/// ## The amplification this exists to stop
///
/// The shed count travels with the
/// first record ACCEPTED after the gap (see [`enqueue_async_into`]), and
/// under a SUSTAINED overload — the documented `VOICEPI_LOG=debug` mouse
/// stream against a stalled AppData volume — every single dequeue frees
/// exactly one slot, the producer refills it immediately, and more
/// records are shed while the writer is still writing. So every record
/// the writer dequeues carries a non-zero count, and a writer that emits
/// a marker for each of them writes nearly ONE MARKER PER SURVIVING
/// RECORD: it doubles the write volume against the very sink that was
/// already too slow, which sheds more records, which emits more markers.
///
/// The fix is to treat an overload as an EPISODE rather than as a
/// property of one record: announce it once when it starts, accumulate
/// silently while it lasts, and summarise it once when the queue catches
/// up. Marker volume becomes O(episodes) instead of O(records) while the
/// two guarantees from the earlier rounds survive:
///
/// * **Nothing is lost.** Every carried count lands in [`Self::total`],
///   and the closing summary names that total. The episode also closes
///   on an idle-queue wakeup, so a burst that ends the trace outright is
///   still reported within one [`ASYNC_PARK_POLL`].
/// * **The start marker keeps its queue position.** It is written
///   immediately ahead of the first record accepted after the gap, which
///   is exactly where the previous round put it — records accepted
///   BEFORE the gap are still written first.
#[derive(Default)]
struct BurstState {
    /// Records shed in the current episode, whether or not a marker has
    /// named them yet. Zero when no episode is open.
    total: u64,
    /// True once the episode-start marker has been written, so the
    /// remaining records of the episode stay silent.
    active: bool,
    /// Consecutive accepted records with nothing shed ahead of them,
    /// counted only while an episode is open.
    clean_run: usize,
}

impl BurstState {
    /// Fold one dequeued record's carried shed count into the episode,
    /// writing AT MOST one marker: the start notice on the first shed of
    /// a new episode, or the summary once [`ASYNC_BURST_CLEAR_RUN`]
    /// clean records say the queue caught up. Called BEFORE the record
    /// itself is written, so either marker keeps its queue position.
    fn observe_record<F>(&mut self, drops_before: u64, capacity: usize, sink: &mut F)
    where
        F: FnMut(&str),
    {
        if drops_before > 0 {
            self.clean_run = 0;
            self.total += drops_before;
            if !self.active {
                self.active = true;
                sink(&async_dropped_marker(drops_before, capacity));
            }
            return;
        }
        if !self.active {
            return;
        }
        self.clean_run += 1;
        if self.clean_run >= ASYNC_BURST_CLEAR_RUN {
            self.close(capacity, sink);
        }
    }

    /// Close the open episode and write its single summary line, then
    /// reset so the next overload reports its own size rather than a
    /// running total.
    ///
    /// When no start marker was ever written (a burst that never got a
    /// record to ride on — the wedge case) the episode has no "above" to
    /// summarise, so the plain [`async_dropped_marker`] is the honest
    /// shape: one line, one gap, one count.
    fn close<F>(&mut self, capacity: usize, sink: &mut F)
    where
        F: FnMut(&str),
    {
        if self.total > 0 {
            let line = if self.active {
                async_burst_summary_marker(self.total, capacity)
            } else {
                async_dropped_marker(self.total, capacity)
            };
            sink(&line);
        }
        *self = Self::default();
    }
}

/// Close the current episode after folding in whatever is still
/// outstanding on the shared counter.
///
/// Called from the two places that KNOW the queue caught up: the
/// [`ASYNC_PARK_POLL`] timeout (the queue is empty) and the loop exit
/// (every sender is gone). `swap` rather than `load` + `store` so a drop
/// racing in between the read and the reset is carried into the NEXT
/// episode instead of being lost: the accounting is allowed to be late,
/// never wrong.
///
/// Deliberately only [`DropLedger::claim_unbound`]: a count already
/// riding on a queued record will name itself when that record is
/// written, and reporting it here as well would double the gap in the
/// log. The teardown close ([`close_burst_with_every_unnamed_drop`]) is
/// the one that has no "later" to defer to.
fn close_burst_with_pending_drops<F>(
    burst: &mut BurstState,
    dropped: &DropLedger,
    capacity: usize,
    sink: &mut F,
) where
    F: FnMut(&str),
{
    burst.total += dropped.claim_unbound();
    burst.close(capacity, sink);
}

/// Close the current episode after folding in EVERY drop no marker has
/// named yet — including counts a producer took before the shutdown
/// sentinel was queued and counts riding on records still behind it.
///
/// ## Why teardown needs a different close
///
/// [`close_burst_with_pending_drops`] reads only the count that is still
/// unbound, on the reasoning that anything already bound to a record will
/// name itself when that record is written. That reasoning holds for
/// every close EXCEPT the drain: a live rdev / raw-hook callback racing
/// teardown can take the outstanding count BEFORE
/// [`drain_and_shutdown_into`] queues its sentinel and enqueue its record
/// AFTER it. The unbound counter then reads zero, the drain is
/// acknowledged, and `main` is free to exit before the post-ack sweep
/// ever reaches that record — so a gap that happened BEFORE the drain
/// request, together with the trace line that resumed after it, is lost
/// on exactly the crash-adjacent exit the drain exists for.
///
/// Asking the ledger what is still UNNAMED closes that hole without
/// waiting for anything: `unnamed` is bumped at shed time and only the
/// writer clears it, so it covers the count wherever it currently sits.
/// The drain still never waits on younger TRAFFIC — this reads two
/// atomics and writes at most one line, exactly like the close it
/// replaces.
fn close_burst_with_every_unnamed_drop<F>(
    burst: &mut BurstState,
    dropped: &DropLedger,
    capacity: usize,
    sink: &mut F,
) where
    F: FnMut(&str),
{
    burst.total += dropped.claim_every_unnamed();
    burst.close(capacity, sink);
}

/// Write one dequeued record: its episode marker first, if this record
/// is the one that opens or closes an episode (so a reader sees the gap
/// announced immediately ahead of the trace that resumed after it), then
/// the line itself.
///
/// `pending` is decremented AFTER the write so a test polling
/// `ASYNC_PENDING == 0` (via [`flush_async_for_tests`]) sees the file has
/// actually been written to.
fn write_async_record<F>(
    drops_before: u64,
    message: &str,
    capacity: usize,
    pending: &AtomicUsize,
    dropped: &DropLedger,
    burst: &mut BurstState,
    sink: &mut F,
) where
    F: FnMut(&str),
{
    // The episode marker about to be written names this count, so it
    // comes off the ledger's "nobody has named this yet" side here and
    // nowhere else.
    dropped.mark_named(drops_before);
    burst.observe_record(drops_before, capacity, sink);
    sink(message);
    pending.fetch_sub(1, Ordering::Relaxed);
}

/// Handle an [`AsyncRecord::Shutdown`] sentinel: close any open
/// overload episode, **acknowledge immediately**, and only then sweep up
/// whatever the producer squeezed in behind the sentinel.
///
/// ## The backlog the caller asked for is ALREADY written
///
/// The sentinel travels the same FIFO queue as the records, so every
/// record enqueued before [`drain_and_shutdown_into`] was called is
/// ordered ahead of it and was therefore written by the loop's `Line`
/// arm before this function was ever entered. Anything still in the
/// channel here is YOUNGER than the drain request - traffic from an
/// input callback that is still firing during teardown (the documented
/// high-rate mouse trace), which no `main` on its way out is waiting
/// for.
///
/// ## Why the ack comes BEFORE the sweep
///
/// The drain acknowledges before sweeping. Sweeping first
/// acked afterwards, bounded by a count (`capacity`). A count is the
/// wrong currency for a deadline: against a slow-but-functional sink,
/// 256 records of post-sentinel traffic can cost far more than the
/// caller's [`crate::entrypoint::DIAG_DRAIN_DEADLINE`], so the caller
/// times out and warns the operator that the tee file is short - on a
/// run where every record the request covered was already durable. The
/// earlier round's infinite sweep had the same failure with a worse
/// constant.
///
/// Acking first removes the sweep from the caller's critical path
/// entirely, which is the only bound that does not depend on how fast
/// the sink happens to be. Nothing the caller asked for is skipped: the
/// FIFO argument above is what makes that safe, and the episode close
/// below still runs BEFORE the ack.
///
/// ## The sweep still happens - on borrowed time
///
/// It keeps its `capacity` budget and its two jobs, now off the
/// deadline:
///
/// * a queue that was full when the sentinel arrived still gets its
///   records written rather than discarded, and
/// * a second concurrent drainer's sentinel is found and acked as soon
///   as it is seen. The channel holds at most `capacity` messages, so
///   that sentinel can never sit more than `capacity` positions behind
///   ours. Dropping it instead would make that drainer's
///   `recv_timeout` report a spurious failure.
///
/// If the process exits mid-sweep, only younger-than-the-request
/// traffic is lost - which is precisely the trade the deadline exists
/// to make.
///
/// [`close_burst_with_every_unnamed_drop`] before the ack is
/// load-bearing, on two counts:
///
/// * Draining in the middle of an overload episode would otherwise drop
///   that episode's [`async_burst_summary_marker`] on the floor, so the
///   last thing the tee file records about a wedged sink would be the
///   episode's OPENING count rather than its total.
/// * It names every drop the ledger still has outstanding, not just the
///   unbound ones, so a reservation a producer took before the sentinel
///   was queued cannot be stranded behind it (see
///   [`close_burst_with_every_unnamed_drop`]).
///
/// It is ONE line against the sink, not a queue's worth, so it cannot
/// blow the budget the way the sweep could.
fn drain_and_ack_shutdown<F>(
    rx: &Receiver<AsyncRecord>,
    ack: Sender<()>,
    capacity: usize,
    pending: &AtomicUsize,
    dropped: &DropLedger,
    burst: &mut BurstState,
    sink: &mut F,
) where
    F: FnMut(&str),
{
    // Part of the requested backlog: the episode summary describes
    // records that were shed BEFORE the drain request - wherever their
    // count currently sits.
    close_burst_with_every_unnamed_drop(burst, dropped, capacity, sink);
    // Best-effort: a drainer that already gave up on its deadline has
    // dropped the receiver.
    let _ = ack.send(());

    let mut budget = capacity;
    while budget > 0 {
        let Ok(queued) = rx.try_recv() else { break };
        budget -= 1;
        match queued {
            // `drops_before` is deliberately DISCARDED here: the close
            // above already named every outstanding drop, including
            // whatever this record was carrying, so folding it in again
            // would report one gap as two.
            AsyncRecord::Line { message, .. } => {
                write_async_record(0, &message, capacity, pending, dropped, burst, sink);
            }
            AsyncRecord::Shutdown(extra) => {
                let _ = extra.send(());
            }
        }
    }
    // The sweep itself runs while the producer is still firing, so it can
    // shed; close again on the same "name everything" terms, because
    // after this the writer is gone.
    close_burst_with_every_unnamed_drop(burst, dropped, capacity, sink);
}

/// The writer thread's whole body, parameterised over the receiver,
/// the two counters and the sink.
///
/// Production calls this from [`ensure_async_writer`] with the
/// process-wide statics and [`write_line`]; `diag_tests` calls it with
/// a tiny channel that was already flooded past its bound and a sink
/// that just collects lines, which is the only way to drive the "queue
/// filled while the sink could not keep up" path at all — the
/// production queue is 256 deep and its sink is a file write no test
/// can pause.
///
/// Loops until every sender is dropped (tests) or an
/// [`AsyncRecord::Shutdown`] sentinel arrives (the shared process exit
/// path, via [`drain_and_shutdown`]). In a shipping process the
/// `OnceLock` keeps a sender alive for the whole run, so the sentinel
/// is the only orderly way out; the trailing
/// [`close_burst_with_pending_drops`] therefore only runs in tests.
///
/// ## Why the park is `recv_timeout` and not `recv`
///
/// Because the sender is immortal, a blocking `recv` parks FOREVER once
/// the queue drains. Records shed after the last surviving record have
/// no "next record" to ride on, so under a plain `recv` a burst that
/// ends the trace is reported only if the trace later resumes — i.e.
/// never, in the wedge case this whole mechanism exists to diagnose
/// Parking with
/// [`ASYNC_PARK_POLL`] turns "reported only if more events arrive" into
/// "reported within half a second, unconditionally"; the timeout arm is
/// the only place a marker can be emitted with no record to attach it
/// to, which is exactly the case that needs it.
///
/// ## Why the loop carries [`BurstState`]
///
/// A marker per dequeued record with a non-zero carried count is nearly
/// one marker per surviving record under a sustained overload, which
/// doubles the load on the sink that was already too slow. The state machine
/// collapses that to one notice
/// when the episode starts plus one summary when the queue catches up.
/// The [`AsyncRecord::Shutdown`] arm closes that episode too, so a drain
/// that lands mid-overload still records the episode total.
pub(crate) fn run_async_writer_loop<F>(
    rx: Receiver<AsyncRecord>,
    capacity: usize,
    pending: &AtomicUsize,
    dropped: &DropLedger,
    mut sink: F,
) where
    F: FnMut(&str),
{
    let mut burst = BurstState::default();
    loop {
        match rx.recv_timeout(ASYNC_PARK_POLL) {
            Ok(AsyncRecord::Line {
                drops_before,
                message,
            }) => write_async_record(
                drops_before,
                &message,
                capacity,
                pending,
                dropped,
                &mut burst,
                &mut sink,
            ),
            // The orderly exit: the backlog queued ahead of the
            // sentinel is already written (FIFO), so sweep up a bounded
            // amount of younger traffic, close any open episode,
            // acknowledge, stop.
            Ok(AsyncRecord::Shutdown(ack)) => {
                drain_and_ack_shutdown(&rx, ack, capacity, pending, dropped, &mut burst, &mut sink);
                return;
            }
            // Parked with an empty queue: the producer is no longer
            // outrunning the sink, so the episode is over — report it,
            // including anything shed that no record will ever carry.
            Err(RecvTimeoutError::Timeout) => {
                close_burst_with_pending_drops(&mut burst, dropped, capacity, &mut sink);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    close_burst_with_pending_drops(&mut burst, dropped, capacity, &mut sink);
}
