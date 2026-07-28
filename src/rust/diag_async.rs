//! Async diagnostic writer — off-loads `crate::diag::log!` calls from
//! latency-sensitive hot paths onto a dedicated writer thread.
//!
//! ## Why this exists (Windows LL-hook callback timeout)
//!
//! Codex P2 #651 discussion PRRT_kwDOSfNjQs6UTvPm: on Windows the rdev
//! listener callback runs on the OS's LL-hook thread and fires for
//! **every** desktop-wide keyboard event. At `VOICEPI_LOG=debug` /
//! `trace`, the callback synchronously calls `crate::diag::log!`
//! multiple times per event, each of which acquires a shared mutex
//! (the writer slot) and flushes the AppData tee file. A slow write
//! (SMB-backed AppData, an antivirus real-time scan, a full disk
//! stall) can easily push the callback past Windows' documented
//! ~300 ms LL-hook timeout — at which point Windows SILENTLY REMOVES
//! the hook, and PTT stops working for the rest of the session.
//! That is the exact wedge the LL-hook diagnostics were built to
//! investigate, so a diagnostic that CAUSES the wedge defeats itself.
//!
//! ## Design — BOUNDED MPSC + one background writer
//!
//! Two pieces:
//!
//! * A [`std::sync::mpsc::SyncSender<WriterMessage>`] bounded to
//!   [`QUEUE_CAPACITY`] records. Callers on the LL-hook thread call
//!   [`enqueue_or_drop`] with a pre-formatted `String`, which does a
//!   **non-blocking** `try_send` and returns immediately — the format
//!   cost is on their stack, the file write is not, and a full queue
//!   never parks the hook thread.
//! * A background writer thread (named `vp-hotkey-rdev-diag-writer`)
//!   that loops on `rx.recv()` and forwards each drained
//!   [`WriterMessage::Line`] to a sink function. Production wires the
//!   sink to [`crate::diag::write_line`]; unit tests supply their own
//!   sink to observe drain order and completeness.
//!
//! ## Why bounded (Codex P2 #675 PRRT_kwDOSfNjQs6Uc5ki)
//!
//! The queue was originally unbounded, on the reasoning that the
//! LL-hook rate is capped by human typing speed. That reasoning was
//! wrong on two counts:
//!
//! 1. The rdev callback enqueues its `raw=` record BEFORE
//!    `raw_from_rdev` filters non-key events, so `MouseMove` — which
//!    fires at the mouse's report rate, hundreds of events/sec while
//!    the user simply moves the pointer — also enters the channel.
//! 2. The whole point of the writer thread is to tolerate a **stalled**
//!    sink (SMB-backed AppData, antivirus real-time scan, full disk).
//!    An unbounded queue in front of a stalled consumer is an
//!    unbounded memory leak: every record retains a formatted `String`
//!    indefinitely, so sustained mouse movement at `VOICEPI_LOG=trace`
//!    grows the process until the GUI is unstable or OOM-killed.
//!
//! So the channel is bounded and sheds load at the tail
//! (drop-newest): `try_send` failing with `Full` bumps a shared
//! counter instead of blocking. The obvious objection to shedding —
//! "did the wedge just happen or did we just drop the line?" — is
//! answered by making every drop **accounted for**: the writer thread
//! emits a single coalesced `dropped=<n>` marker (see
//! [`dropped_marker`]) before the next record it writes, so a reader
//! of the tee file always knows the trace has a gap and how big it
//! was. One marker per burst, never one line per drop — a per-drop
//! marker would itself be an unbounded write amplification against
//! the same stalled sink.
//!
//! Drop-newest rather than drop-oldest because the records already in
//! the queue are the ones closest to the *start* of the burst, which
//! is where the wedge signal lives; the tail of a mouse-move flood is
//! the least interesting part of the trace.
//!
//! ## Shutdown (Codex P2 #675 PRRT_kwDOSfNjQs6UbAiW)
//!
//! [`drain_and_shutdown`] sends a [`WriterMessage::Shutdown`] sentinel
//! carrying an ack sender. When the writer thread pops the sentinel it
//! first drains every remaining `Line` from the channel, then signals
//! the ack. The GUI binary calls this AFTER its UI loop returns so a
//! wedge repro captured by the operator is durable in the tee file
//! before the process exits — a bare `main` return would kill the
//! background writer thread and lose whatever was still in the queue.
//! The deadline argument bounds the wait so a stuck writer cannot
//! block the process from exiting.
//!
//! ## Startup failure surfaces to the caller (Codex P2 #675 PRRT_kwDOSfNjQs6UbAip)
//!
//! [`writer_result`] returns `Result<&Sender, String>` so the rdev
//! listener spawn path can propagate a thread-spawn failure via its
//! `ListenerSignal::Failed` channel BEFORE announcing readiness.
//! Without this, a `Builder::spawn` failure inside the writer would
//! surface as a panicked writer thread that silently drops every
//! diagnostic record for the rest of the process's lifetime while the
//! manager reported a successful hotkey installation.
//!
//! ## Contract
//!
//! * [`writer_result`] returns the process-wide [`WriterHandle`],
//!   initialising the writer thread on first call. `OnceLock`-backed
//!   so a hot-path caller pays only a single relaxed load after
//!   startup. Result variant so a thread-spawn failure at init is
//!   propagated instead of panicked.
//! * [`writer`] is the infallible-Option wrapper for hot-path callers
//!   that don't care about the error text.
//! * [`enqueue_or_drop`] is the one-liner every hot-path call site
//!   uses. It looks up the writer, sends a [`WriterMessage::Line`],
//!   and silently drops the record if the writer failed to init or the
//!   thread has stopped. We never want a diagnostic write to panic the
//!   LL-hook thread.
//! * [`enqueue`] is the test seam: unit tests spawn their own writer
//!   with a mutex-guarded `Vec<String>` sink and drive it directly.
//! * [`spawn_writer_with_sink`] is the test seam: the unit tests
//!   spawn their own writer with a mutex-guarded `Vec<String>` sink
//!   and drive it directly, so the async pipeline is testable without
//!   depending on the global diag writer slot or a temp file.
//!
//! ## Non-goals
//!
//! Not a general-purpose log framework. There is no level filter (the
//! caller has already decided to emit), no batching, no rotation. The
//! sole job is to make sure a slow file write cannot stall the LL-hook
//! callback.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// Maximum number of diagnostic records buffered ahead of the writer
/// thread. Past this the queue sheds the newest record and bumps the
/// dropped counter instead of blocking the LL-hook callback.
///
/// Sized so the worst realistic burst — `VOICEPI_LOG=trace` with a
/// mouse flooding `MouseMove` at ~1 kHz — buys the writer several
/// seconds of head start, while the memory ceiling stays trivial: a
/// debug/trace record is a few hundred bytes, so 4096 of them is
/// low-single-digit megabytes even at the top of the range. That is a
/// *bound*; the pre-fix unbounded channel had none at all.
pub(crate) const QUEUE_CAPACITY: usize = 4096;

/// How long [`drain_and_shutdown`] naps between `try_send` attempts
/// while the bounded queue is full. Short enough that a queue draining
/// normally costs no measurable teardown latency, long enough that a
/// genuinely wedged writer isn't spun on.
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Envelope for the internal writer channel. `Line` carries a
/// pre-formatted diagnostic record; `Shutdown` is a sentinel that
/// tells the writer thread to drain any remaining records and then
/// signal the paired ack sender before exiting.
pub(crate) enum WriterMessage {
    /// A diagnostic record to hand off to the sink.
    Line(String),
    /// Ask the writer to drain and exit, then send `()` on the paired
    /// ack sender. Used by [`drain_and_shutdown`] so the GUI can wait
    /// for the queue to flush before returning from `main`.
    Shutdown(Sender<()>),
}

/// Sender end of the bounded writer channel plus the shared
/// drop-accounting counter.
///
/// Bundled into one type (rather than passing a bare `SyncSender`
/// around) so the counter the enqueue path bumps and the counter the
/// writer thread reads can never drift apart: [`spawn_writer_with_sink`]
/// is the sole constructor and hands one `Arc` to each side.
pub(crate) struct WriterHandle {
    tx: SyncSender<WriterMessage>,
    /// Records shed since the writer last emitted a `dropped=` marker.
    /// Bumped by [`WriterHandle::enqueue_or_drop`] on the LL-hook
    /// thread (a single relaxed `fetch_add`, no allocation, no lock)
    /// and `swap`ped back to zero by the writer thread when it emits
    /// the coalesced marker.
    dropped: Arc<AtomicU64>,
}

impl WriterHandle {
    /// Hand `msg` to the writer thread without ever blocking.
    ///
    /// This runs on the Windows `WH_KEYBOARD_LL` callback thread, so
    /// the only acceptable outcomes are "queued" and "shed" — never
    /// "parked". A full queue bumps [`WriterHandle::dropped`]; a dead
    /// writer is ignored entirely (there is nobody left to tell, and
    /// panicking here would take the hook thread down).
    pub(crate) fn enqueue_or_drop(&self, msg: String) {
        match self.tx.try_send(WriterMessage::Line(msg)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                // Codex P2 #675 PRRT_kwDOSfNjQs6Uc5ki: shed the newest
                // record but ACCOUNT for it, so the writer can tell the
                // log reader the trace has a gap.
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                // Writer thread is gone (post-`drain_and_shutdown`, or
                // it panicked). Not counted: the marker could never be
                // emitted, so a counter here would only grow forever.
            }
        }
    }

    /// Records shed since the writer last emitted a marker. Test-only
    /// observation point for the shedding contract.
    #[cfg(test)]
    pub(crate) fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Blocking `Shutdown` send for the companion tests, which drive
    /// the drain-then-ack contract against a locally-spawned writer
    /// rather than the process-wide `OnceLock` slot (an `OnceLock` has
    /// no `take`, so tests can never own the production writer).
    #[cfg(test)]
    pub(crate) fn send_shutdown_for_tests(&self, ack: Sender<()>) -> bool {
        self.tx.send(WriterMessage::Shutdown(ack)).is_ok()
    }

    /// Wrap a caller-owned `SyncSender` so tests can drive
    /// [`WriterHandle::enqueue_or_drop`] against a channel whose
    /// receiver they control (including one they have already
    /// dropped).
    #[cfg(test)]
    pub(crate) fn from_sender_for_tests(tx: SyncSender<WriterMessage>) -> Self {
        Self {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// The coalesced load-shed marker the writer emits before the next
/// record it writes after one or more drops.
///
/// Pulled out as a named function (rather than inlined into the writer
/// loop) so the regression test can assert on the exact shape without
/// duplicating the format string, and so a future log-parsing tool has
/// one place to match.
pub(crate) fn dropped_marker(dropped: u64, capacity: usize) -> String {
    format!(
        // ASCII only: this string reaches stderr via `diag::write_line`,
        // and typographic punctuation garbles under cmd.exe on a legacy
        // code page (AGENTS.md; pinned by `console_ascii_tests`).
        "[diag-async] dropped={dropped} record(s): the diagnostic queue \
         (capacity={capacity}) filled while the sink was slow - the trace \
         below has a gap"
    )
}

/// Process-wide handle for the async diagnostic writer. Initialised
/// on first [`writer_result`] call via `OnceLock::get_or_init`, then
/// read on every subsequent call with a single relaxed load. Never
/// dropped in production; the writer thread runs for the process
/// lifetime unless [`drain_and_shutdown`] is called at teardown.
///
/// Stored as `Result<WriterHandle, String>` so a thread-spawn failure
/// at init is remembered and surfaced through [`writer_result`] rather
/// than panicked — Codex P2 #675 PRRT_kwDOSfNjQs6UbAip.
static WRITER: OnceLock<Result<WriterHandle, String>> = OnceLock::new();

/// Get (initialising on first call) the process-wide async-writer
/// sender. Cheap on the hot path — the `OnceLock` is loaded with a
/// relaxed atomic once initialised. Only the very first call spawns
/// the writer thread; every subsequent caller gets the cached
/// sender.
///
/// Returns `Err` if the writer thread could not be spawned (e.g. the
/// OS refused thread creation). The rdev listener spawn path uses
/// this variant to propagate the failure via its `ListenerSignal`
/// channel BEFORE announcing readiness — otherwise a
/// `Builder::spawn` failure inside the writer would surface as a
/// panicked writer thread that silently drops every diagnostic
/// record. Codex P2 #675 PRRT_kwDOSfNjQs6UbAip.
pub(crate) fn writer_result() -> Result<&'static WriterHandle, String> {
    match WRITER.get_or_init(|| spawn_writer_with_sink(crate::diag::write_line)) {
        Ok(handle) => Ok(handle),
        Err(msg) => Err(msg.clone()),
    }
}

/// Cheap infallible wrapper around [`writer_result`] for the hot
/// path — returns `None` if the writer failed to init. Every
/// production caller uses [`enqueue_or_drop`] which internally goes
/// through this and silently drops the record when the writer is
/// unavailable; the standalone `writer` accessor is kept for tests
/// and for future callers that want to observe the availability
/// signal without allocating.
pub(crate) fn writer() -> Option<&'static WriterHandle> {
    writer_result().ok()
}

/// Send `msg` through the process-wide async writer, silently
/// dropping the record if the writer is unavailable or its thread
/// has stopped. This is the one-liner every hot-path call site uses.
/// Called with a pre-formatted `String` so the caller pays the format
/// cost on their own stack (cheap, on-stack `format!`) and the file
/// write happens on the writer thread (potentially slow, off the
/// LL-hook budget).
pub(crate) fn enqueue_or_drop(msg: String) {
    if let Some(handle) = writer() {
        handle.enqueue_or_drop(msg);
    }
}

/// Send `msg` on a test-owned writer sender. Kept as a test-only
/// seam so the companion tests can drive [`spawn_writer_with_sink`]
/// without going through the process-wide `OnceLock`. Ignores send
/// errors for the same reason as [`enqueue_or_drop`] — a dead
/// consumer is treated as "the record is not needed", never as a
/// fatal error.
#[cfg(test)]
pub(crate) fn enqueue(handle: &WriterHandle, msg: String) {
    handle.enqueue_or_drop(msg);
}

/// Drain any queued diagnostic records and stop the writer thread.
///
/// Sends a [`WriterMessage::Shutdown`] carrying an ack sender; the
/// writer thread pops it, drains every remaining `Line` from the
/// channel, forwards them to the sink, and signals the ack before
/// returning. `drain_and_shutdown` blocks up to `deadline` waiting
/// for the ack so a stuck writer cannot pin the process from exiting.
///
/// Returns `true` when the writer acknowledged the drain within the
/// deadline (or when the writer was never initialised — nothing to
/// drain), `false` on timeout / a disconnected channel. The GUI's
/// `main` calls this after its UI loop returns and does not currently
/// act on the return value — the tee-file write is best-effort — but
/// tests use the boolean to assert the drain completed. Codex P2 #675
/// PRRT_kwDOSfNjQs6UbAiW.
pub fn drain_and_shutdown(deadline: Duration) -> bool {
    let Some(handle) = writer() else {
        return true;
    };
    let started = Instant::now();
    let (ack_tx, ack_rx) = mpsc::channel::<()>();
    // The channel is bounded now, so a plain `send` here could park
    // forever behind a stalled writer with a full queue — exactly the
    // teardown hang the deadline exists to prevent. `SyncSender::
    // send_timeout` is still unstable, so poll `try_send` (which hands
    // the message back on `Full`) inside the same overall budget.
    let mut sentinel = WriterMessage::Shutdown(ack_tx);
    loop {
        match handle.tx.try_send(sentinel) {
            Ok(()) => break,
            // Writer thread already exited — nothing to drain.
            Err(TrySendError::Disconnected(_)) => return true,
            Err(TrySendError::Full(returned)) => {
                let remaining = deadline.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    // Queue stayed full for the whole budget: the
                    // writer is wedged and the caller (a `main` on its
                    // way out) must not wait any longer.
                    return false;
                }
                sentinel = returned;
                thread::sleep(SHUTDOWN_POLL_INTERVAL.min(remaining));
            }
        }
    }
    let remaining = deadline.saturating_sub(started.elapsed());
    ack_rx.recv_timeout(remaining).is_ok()
}

/// Spawn a writer thread bound to `sink`. Returns the sender end of
/// the channel or a stringified spawn error. Used by [`writer_result`]
/// to wire up the production sink and by the unit tests to observe
/// drain order with a mutex-guarded `Vec`.
///
/// The thread is named so a Windows dump / a `taskkill /f /t`
/// trace names it, and so `Thread::current().name()` in a future
/// panic hook can attribute stack traces to the right subsystem.
///
/// A spawn failure is returned as `Err(msg)` rather than panicked so
/// [`writer_result`]'s cached `Result` can surface the failure to the
/// rdev listener spawn path via its `ListenerSignal::Failed` channel
/// — Codex P2 #675 PRRT_kwDOSfNjQs6UbAip. The previous version
/// `.expect`-ed the spawn, which turned a survivable "diagnostic
/// disabled" outcome into a panic that took down the LL-hook
/// listener thread with no bearing on PTT itself.
pub(crate) fn spawn_writer_with_sink<F>(sink: F) -> Result<WriterHandle, String>
where
    F: FnMut(&str) + Send + 'static,
{
    spawn_writer_with_capacity(QUEUE_CAPACITY, sink)
}

/// [`spawn_writer_with_sink`] with an explicit channel bound.
///
/// Production always uses [`QUEUE_CAPACITY`]; the parametrised form
/// exists so the load-shedding regression test can fill the queue with
/// a handful of records instead of thousands, keeping the test fast
/// and its arithmetic exact.
///
/// `capacity` is clamped to at least 1: `mpsc::sync_channel(0)` is a
/// *rendezvous* channel where `try_send` fails unless a receiver is
/// already parked, which would shed almost every record.
pub(crate) fn spawn_writer_with_capacity<F>(
    capacity: usize,
    mut sink: F,
) -> Result<WriterHandle, String>
where
    F: FnMut(&str) + Send + 'static,
{
    let capacity = capacity.max(1);
    let (tx, rx) = mpsc::sync_channel::<WriterMessage>(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    let dropped_writer = Arc::clone(&dropped);
    thread::Builder::new()
        .name("vp-hotkey-rdev-diag-writer".to_owned())
        .spawn(move || {
            // `recv()` blocks until a record arrives or the last
            // sender is dropped. In production the sender lives in
            // a static `OnceLock` and is never dropped, so this
            // loop runs for the process lifetime — unless a binary's
            // exit path calls [`drain_and_shutdown`] on teardown,
            // which sends a `Shutdown` sentinel below.
            while let Ok(msg) = rx.recv() {
                match msg {
                    WriterMessage::Line(line) => {
                        emit_pending_drops(&dropped_writer, capacity, &mut sink);
                        sink(&line);
                    }
                    WriterMessage::Shutdown(ack) => {
                        // Drain any remaining `Line` records BEFORE
                        // signalling the ack so callers get an
                        // "everything landed" acknowledgement. Any
                        // `Shutdown` sentinel received here is
                        // spurious — the process should send at most
                        // one — but we forward the ack anyway so a
                        // buggy caller doesn't deadlock.
                        while let Ok(pending) = rx.try_recv() {
                            match pending {
                                WriterMessage::Line(line) => {
                                    emit_pending_drops(&dropped_writer, capacity, &mut sink);
                                    sink(&line);
                                }
                                WriterMessage::Shutdown(spurious) => {
                                    let _ = spurious.send(());
                                }
                            }
                        }
                        // Final marker: records shed during the drain
                        // itself would otherwise never be reported,
                        // because there is no "next record" to prefix.
                        emit_pending_drops(&dropped_writer, capacity, &mut sink);
                        let _ = ack.send(());
                        return;
                    }
                }
            }
        })
        .map(|_join| WriterHandle { tx, dropped })
        .map_err(|e| format!("spawn diag async writer thread failed: {e}"))
}

/// Emit the coalesced [`dropped_marker`] if anything was shed since
/// the last time this ran, and reset the counter.
///
/// `swap` rather than `load` + `store` so a drop racing in between the
/// read and the reset is carried into the *next* marker instead of
/// being lost — the accounting is allowed to be late, never wrong.
fn emit_pending_drops<F>(dropped: &AtomicU64, capacity: usize, sink: &mut F)
where
    F: FnMut(&str),
{
    let shed = dropped.swap(0, Ordering::Relaxed);
    if shed > 0 {
        sink(&dropped_marker(shed, capacity));
    }
}
