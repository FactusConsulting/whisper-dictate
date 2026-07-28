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
//! * A [`std::sync::mpsc::Sender<String>`] whose `send` is lock-free on
//!   the sender side (no mutex acquisition; the channel manages
//!   ordering internally). Callers on the LL-hook thread `enqueue` a
//!   pre-formatted `String` and return immediately — the format cost
//!   is on their stack, the file write is not.
//! * A background writer thread (named `vp-hotkey-rdev-diag-writer`)
//!   that loops on `rx.recv()` and forwards each drained record to a
//!   sink function. Production wires the sink to
//!   [`crate::diag::write_line`]; unit tests supply their own sink to
//!   observe drain order and completeness.
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
//! ## Contract
//!
//! * [`writer`] returns the process-wide sender, initialising the
//!   writer thread on first call. `OnceLock`-backed so a hot-path
//!   caller pays only a single relaxed load after startup.
//! * [`enqueue`] is a wrapper around `Sender::send` that ignores
//!   send errors. A dropped writer thread (only possible in a test
//!   harness that installed its own writer and then dropped the
//!   sender) is treated as "the record is not needed", not a fatal
//!   error — we never want a diagnostic write to panic the
//!   LL-hook thread.
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

/// Process-wide sender for the async diagnostic writer. Initialised
/// on first [`writer`] call via `OnceLock::get_or_init`, then read
/// on every subsequent call with a single relaxed load. Never
/// dropped in production; the writer thread runs for the process
/// lifetime.
static WRITER: OnceLock<Sender<String>> = OnceLock::new();

/// Get (initialising on first call) the process-wide async-writer
/// sender. Cheap on the hot path — the `OnceLock` is loaded with a
/// relaxed atomic once initialised. Only the very first call spawns
/// the writer thread; every subsequent caller gets the cached
/// sender.
///
/// Production wires the writer thread to
/// [`crate::diag::write_line`] so every enqueued record eventually
/// lands in `gui-diagnostic.log` the same way a synchronous
/// `crate::diag::log!` call would — the only difference is
/// ordering-with-respect-to-the-emitting-thread (asynchronous vs.
/// synchronous). The LL-hook callback benefits; nothing else about
/// the diagnostic contract changes.
pub(crate) fn writer() -> &'static Sender<String> {
    WRITER.get_or_init(|| spawn_writer_with_sink(crate::diag::write_line))
}

/// Send `msg` to the async writer, silently ignoring send errors.
/// A dropped writer thread is treated as "the record is not needed"
/// — this is called from the LL-hook callback which must never
/// panic. Called with a pre-formatted `String` so the caller pays
/// the format cost on their own stack (cheap, on-stack `format!`)
/// and the file write happens on the writer thread (potentially
/// slow, off the LL-hook budget).
pub(crate) fn enqueue(sender: &Sender<String>, msg: String) {
    let _ = sender.send(msg);
}

/// Spawn a writer thread bound to `sink`. Returns the sender end of
/// the channel. Used by [`writer`] to wire up the production sink
/// and by the unit tests to observe drain order with a
/// mutex-guarded `Vec`.
///
/// The thread is named so a Windows dump / a `taskkill /f /t`
/// trace names it, and so `Thread::current().name()` in a future
/// panic hook can attribute stack traces to the right subsystem.
pub(crate) fn spawn_writer_with_sink<F>(mut sink: F) -> Sender<String>
where
    F: FnMut(&str) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<String>();
    // The writer thread's spawn is `.expect`-ed rather than fallibly
    // surfaced because a failure to spawn ANY thread this early in
    // process lifetime indicates the OS is refusing thread creation
    // — the whole runtime is going to be unhealthy and a hard
    // failure here surfaces the root cause loudly rather than
    // silently dropping every diagnostic line for the rest of the
    // session.
    thread::Builder::new()
        .name("vp-hotkey-rdev-diag-writer".to_owned())
        .spawn(move || {
            // `recv()` blocks until a record arrives or the last
            // sender is dropped. In production the sender lives in
            // a static `OnceLock` and is never dropped, so this
            // loop runs for the process lifetime. In tests the
            // sender is dropped when the test function returns,
            // which cleanly ends the thread.
            while let Ok(msg) = rx.recv() {
                sink(&msg);
            }
        })
        .expect("spawn diag async writer thread");
    tx
}
