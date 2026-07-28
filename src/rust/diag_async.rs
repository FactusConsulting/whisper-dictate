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
//! ## Design — unbounded MPSC + one background writer
//!
//! Two pieces:
//!
//! * A [`std::sync::mpsc::Sender<WriterMessage>`] whose `send` is
//!   lock-free on the sender side (no mutex acquisition; the channel
//!   manages ordering internally). Callers on the LL-hook thread call
//!   [`enqueue_or_drop`] with a pre-formatted `String` and return
//!   immediately — the format cost is on their stack, the file write
//!   is not.
//! * A background writer thread (named `vp-hotkey-rdev-diag-writer`)
//!   that loops on `rx.recv()` and forwards each drained
//!   [`WriterMessage::Line`] to a sink function. Production wires the
//!   sink to [`crate::diag::write_line`]; unit tests supply their own
//!   sink to observe drain order and completeness.
//!
//! Unbounded is safe here because the writer thread is the sole
//! consumer, the OS's LL-hook rate is bounded by human typing speed
//! (hundreds of events/sec at most under a stress test, single-digit
//! events/sec in ordinary use), and even the largest debug/trace log
//! line is a few hundred bytes — a wedge that leaked memory would
//! take hours to matter, by which time the operator has long since
//! captured the wedge signal. Bounded channels were considered but
//! rejected because `try_send` on a full channel would drop records
//! silently and reintroduce the "did the wedge just happen or did we
//! just drop the line?" ambiguity that motivated the trace in the
//! first place.
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
//! * [`writer_result`] returns the process-wide sender, initialising
//!   the writer thread on first call. `OnceLock`-backed so a hot-path
//!   caller pays only a single relaxed load after startup. Result
//!   variant so a thread-spawn failure at init is propagated instead
//!   of panicked.
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

use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

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

/// Process-wide sender for the async diagnostic writer. Initialised
/// on first [`writer_result`] call via `OnceLock::get_or_init`, then
/// read on every subsequent call with a single relaxed load. Never
/// dropped in production; the writer thread runs for the process
/// lifetime unless [`drain_and_shutdown`] is called at teardown.
///
/// Stored as `Result<Sender, String>` so a thread-spawn failure at
/// init is remembered and surfaced through [`writer_result`] rather
/// than panicked — Codex P2 #675 PRRT_kwDOSfNjQs6UbAip.
static WRITER: OnceLock<Result<Sender<WriterMessage>, String>> = OnceLock::new();

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
pub(crate) fn writer_result() -> Result<&'static Sender<WriterMessage>, String> {
    match WRITER.get_or_init(|| spawn_writer_with_sink(crate::diag::write_line)) {
        Ok(tx) => Ok(tx),
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
pub(crate) fn writer() -> Option<&'static Sender<WriterMessage>> {
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
    if let Some(tx) = writer() {
        let _ = tx.send(WriterMessage::Line(msg));
    }
}

/// Send `msg` on a test-owned writer sender. Kept as a test-only
/// seam so the companion tests can drive [`spawn_writer_with_sink`]
/// without going through the process-wide `OnceLock`. Ignores send
/// errors for the same reason as [`enqueue_or_drop`] — a dead
/// consumer is treated as "the record is not needed", never as a
/// fatal error.
#[cfg(test)]
pub(crate) fn enqueue(sender: &Sender<WriterMessage>, msg: String) {
    let _ = sender.send(WriterMessage::Line(msg));
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
    let Some(tx) = writer() else {
        return true;
    };
    let (ack_tx, ack_rx) = mpsc::channel::<()>();
    if tx.send(WriterMessage::Shutdown(ack_tx)).is_err() {
        // Writer thread already exited — nothing to drain.
        return true;
    }
    ack_rx.recv_timeout(deadline).is_ok()
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
pub(crate) fn spawn_writer_with_sink<F>(mut sink: F) -> Result<Sender<WriterMessage>, String>
where
    F: FnMut(&str) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<WriterMessage>();
    thread::Builder::new()
        .name("vp-hotkey-rdev-diag-writer".to_owned())
        .spawn(move || {
            // `recv()` blocks until a record arrives or the last
            // sender is dropped. In production the sender lives in
            // a static `OnceLock` and is never dropped, so this
            // loop runs for the process lifetime — unless the GUI
            // main calls [`drain_and_shutdown`] on teardown, which
            // sends a `Shutdown` sentinel below.
            while let Ok(msg) = rx.recv() {
                match msg {
                    WriterMessage::Line(line) => sink(&line),
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
                                WriterMessage::Line(line) => sink(&line),
                                WriterMessage::Shutdown(spurious) => {
                                    let _ = spurious.send(());
                                }
                            }
                        }
                        let _ = ack.send(());
                        return;
                    }
                }
            }
        })
        .map(|_join| tx)
        .map_err(|e| format!("spawn diag async writer thread failed: {e}"))
}
