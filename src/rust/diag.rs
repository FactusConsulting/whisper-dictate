//! Diagnostic levels, a shared tee sink and nonblocking callback logging.
//!
//! The Windows GUI owns one diagnostic sink. Callback producers never perform
//! file I/O: they share a bounded queue, drop ledger and shutdown admission gate.
//! Panic reporting has a separate channel. Startup and bounded drain failures
//! remain visible through the same stable crate::diag API.

pub(crate) use crate::diag_drop_ledger::DropLedger;
use crate::diag_shutdown_gate::ShutdownGate;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::{Sender, SyncSender};
use std::sync::OnceLock;
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

mod config;
mod logger;
mod panic;
mod queue;
mod shutdown;
mod startup;
mod writer;

pub(crate) use config::configure_level;
pub use config::{
    callback_diagnostics_enabled, current_level, debug_enabled, info_enabled, init_from_env,
    trace_enabled, LogLevel, LOG_ENV_VAR,
};
#[cfg(test)]
#[allow(unused_imports)] // Used by the opt-in hotkey tests as well as this facade.
pub(crate) use config::{reset_level_for_tests, set_level_for_tests};
pub use logger::{
    default_gui_diagnostic_path, install_gui_diagnostic_log, write_line, write_line_nonblocking,
    write_line_stderr_only,
};
#[cfg(test)]
pub(crate) use logger::{install_tee_sink_for_tests, tee_mutex_for_tests};
#[allow(unused_imports)] // Preserve crate-visible sink seams across feature sets.
pub(crate) use logger::{write_line_to, write_line_to_nonblocking, write_line_to_stderr_only};
#[allow(unused_imports)] // Stable formatter seam, exercised by companion tests.
pub(crate) use panic::format_panic_report;
pub use panic::{drain_panic_reports, install_gui_panic_hook};
pub use queue::enqueue_async;
#[allow(unused_imports)] // Stable producer seams, exercised by companion tests.
pub(crate) use queue::{enqueue_async_into, enqueue_async_into_after};
pub use shutdown::drain_and_shutdown;
#[allow(unused_imports)] // Stable drain seams, exercised by companion tests.
pub(crate) use shutdown::{drain_and_shutdown_into, drain_and_shutdown_into_after};
#[allow(unused_imports)] // Stable spawn-result seams, exercised by companion tests.
pub(crate) use startup::{
    ascii_escaped, record_writer_spawn_outcome, writer_spawn_failure_message,
};
pub use startup::{async_writer_installed, async_writer_result, ensure_async_writer};
#[allow(unused_imports)] // Production uses the loop; tests also use the marker seams.
pub(crate) use writer::{
    async_burst_summary_marker, async_dropped_marker, run_async_writer_loop, ASYNC_BURST_CLEAR_RUN,
};

// ---------------------------------------------------------------------------
// Off-callback async sink.
//
// On Windows the `WH_KEYBOARD_LL` callback runs synchronously in the
// OS input thread and its total time budget (per the low-level hook
// contract) is a few milliseconds before Windows silently unhooks the
// callback — which recreates the exact PTT wedge this instrumentation
// was written to diagnose. So EVERY diagnostic path that fires from
// inside the callback MUST go through a bounded, non-blocking queue,
// not the synchronous `write_line` above.
//
// This queue was originally introduced inside `rdev_driver` for the
// rdev boundary trace only, but the tracker's `[chord]` debug trace also runs
// on that callback thread (`dispatch_raw_event` -> `KeyTracker::handle` ->
// `crate::diag::log!`). Consolidating the queue in `crate::diag` means
// both call sites feed the same writer thread and any future callsite
// on the LL-hook path gets the same protection.
// ---------------------------------------------------------------------------

/// Bounded queue capacity for [`enqueue_async`]. 256 lines absorbs
/// realistic bursts (rate-limited by the callers) even with a stalled
/// AppData volume; a genuine flood dropping lines is preferable to a
/// wedged hook.
///
/// ## Why this stays at 256 now that shedding is *visible*
///
/// The obvious follow-on to the drop accounting below is "the queue is
/// too small, raise it". Deliberately not done here, for four reasons:
///
/// 1. No finite bound survives the worst case anyway. The rdev callback
///    emits its `raw=` line BEFORE `raw_from_rdev` filters non-key
///    events, so at `VOICEPI_LOG=debug` a mouse reporting at ~1 kHz
///    feeds the queue continuously. Against a genuinely stalled sink a
///    4096-deep queue fills in ~4 s instead of ~256 ms: it moves the
///    threshold, it does not remove the failure mode.
/// 2. A deeper queue would MASK the signal this change just added. The
///    `dropped=` marker is the evidence that tells us empirically
///    whether 256 is too small on a real Windows box; sizing up in the
///    same commit that starts measuring throws away the measurement.
/// 3. Queue depth is trace staleness. This is a wedge-TIMING
///    diagnostic: the operator correlates `t=<ms>` prefixes against the
///    moment PTT died. A 4096-deep backlog can put seconds between the
///    event and its log line, which is the wrong trade for the one
///    question the log exists to answer.
/// 4. 256 records is a ~50 KB ceiling, so the memory argument for
///    growing it is weak in both directions.
///
/// If field evidence (a `dropped=` marker with a large `n` on an
/// otherwise healthy host) says otherwise, raising this is a one-line,
/// now-measurable adjustment.
pub const ASYNC_QUEUE_CAPACITY: usize = 256;

/// Process-wide sender to the off-callback trace writer thread. `None`
/// until [`ensure_async_writer`] runs (from the first callback-path
/// caller); once installed it persists for the process lifetime because
/// the writer thread cannot be cleanly torn down (the OS listener that
/// feeds it is itself unjoinable) - except on the shared exit path,
/// where [`drain_and_shutdown`] stops it deliberately.
static ASYNC_QUEUE_TX: OnceLock<SyncSender<AsyncRecord>> = OnceLock::new();

/// How long the writer parks on an empty queue before waking up to check
/// whether anything was shed after the last record it wrote.
///
/// A plain blocking `recv()` here is a correctness bug, not a style
/// choice: the process-wide
/// [`ASYNC_QUEUE_TX`] is never dropped, so a writer that drains the
/// queue and parks stays parked until the NEXT record arrives. A burst
/// shed at the very end of a trace — precisely the "the callback wedged
/// and then nothing else ever happened" case this accounting exists to
/// diagnose — would then never be reported at all. Waking periodically
/// bounds the report latency at one poll interval instead of "forever,
/// unless the wedge un-wedges".
///
/// 500 ms is chosen against both ends: a `dropped=` marker is a
/// post-mortem artefact read minutes later, so half a second of latency
/// is invisible, while 2 idle wakeups per second on one background
/// thread is far below the noise floor of a process that also runs an
/// OS keyboard listener and an egui event loop. Each wakeup does one
/// relaxed atomic load and goes straight back to sleep.
const ASYNC_PARK_POLL: Duration = Duration::from_millis(500);

/// One item in the off-callback queue.
///
/// Two things travel through this one channel, and both need to:
///
/// * Records carry the shed count that preceded them rather than the
///   writer reading a global counter at write time, so the coalesced
///   marker lands at the QUEUE POSITION of the gap instead of at
///   whatever position the writer happened to reach first. See
///   [`enqueue_async_into`] for why that
///   distinction matters.
/// * A plain record-only channel cannot express "stop": the sender
///   lives in a process-wide [`OnceLock`] that is never dropped, so the
///   writer's receive never returns `Disconnected` and the thread is
///   only ever killed by process exit - with whatever was still queued.
///   [`Shutdown`] is the in-band sentinel [`drain_and_shutdown`] pushes
///   so the writer can flush the remaining records and then acknowledge.
///
/// [`Shutdown`]: AsyncRecord::Shutdown
pub(crate) enum AsyncRecord {
    /// One trace line, plus the number of records shed immediately
    /// before this one was accepted (`0` on every healthy enqueue).
    Line { drops_before: u64, message: String },
    /// Drain everything still queued, then signal the paired ack
    /// sender and stop. Ordering is what makes the drain meaningful:
    /// the sentinel travels through the SAME queue as the records, so
    /// every record enqueued before the drain started is necessarily
    /// ahead of it.
    Shutdown(Sender<()>),
}

/// Number of async messages that have been enqueued but not yet
/// written by the writer thread. Bumped in [`enqueue_async`] on a
/// successful `try_send`, decremented by the writer thread after each
/// [`write_line`] returns. Used ONLY by
/// [`flush_async_for_tests`] to wait for the queue to drain before
/// reading the tee file; production code never reads it (the queue is
/// deliberately fire-and-forget so a slow disk cannot back-pressure
/// the LL-hook callback).
static ASYNC_PENDING: AtomicUsize = AtomicUsize::new(0);

/// Process-wide drop accounting for [`enqueue_async`].
static ASYNC_DROPPED: DropLedger = DropLedger::new();

/// Process-wide admission gate for [`enqueue_async`], closed by
/// [`drain_and_shutdown_into`] before it starts polling for sentinel
/// space.
///
/// Without it a producer that keeps firing through teardown - the rdev /
/// raw-hook callback thread is unjoinable, and the documented
/// `VOICEPI_LOG=debug` mouse trace offers a record per millisecond - takes
/// every slot the writer frees before the teardown thread wakes to retry,
/// so the sentinel starves for the whole deadline against a slow sink. See
/// [`crate::diag_shutdown_gate`] for the full argument.
static ASYNC_GATE: ShutdownGate = ShutdownGate::new();

/// Records shed by [`enqueue_async`] that the writer has not yet
/// reported as an [`async_dropped_marker`] line.
///
/// Test-only (mirrors [`async_writer_installed`]'s shape but stays
/// `#[cfg(test)]`): production has no use for the raw number because
/// the marker in the log IS the product, and exposing a counter that
/// the writer resets under the reader's feet would invite call sites
/// that treat a transient zero as "nothing was ever dropped".
#[cfg(test)]
pub(crate) fn async_dropped_count() -> u64 {
    ASYNC_DROPPED.unbound()
}

/// Test-only: block until the async writer has drained every message
/// enqueued so far. Returns as soon as [`ASYNC_PENDING`] reaches zero
/// OR after `timeout` — whichever comes first. Callers that read the
/// tee file after enqueuing must invoke this first, otherwise the
/// file may not yet contain the just-enqueued line.
///
/// A 500 ms timeout is generous — the writer thread's per-message
/// work is a couple of `writeln!` calls and a `flush()`, so a healthy
/// drain of a handful of messages typically completes in single-digit
/// milliseconds. If the writer thread failed to spawn (rare, out of
/// FDs), the call returns after the timeout without observing
/// drainage — tests will then see an empty file and fail loudly.
#[cfg(test)]
pub fn flush_async_for_tests() {
    let deadline = Instant::now() + std::time::Duration::from_millis(500);
    while Instant::now() < deadline && ASYNC_PENDING.load(std::sync::atomic::Ordering::Relaxed) > 0
    {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Off-callback variant of the [`log!`] macro. Formats the arguments
/// once and hands the resulting `String` to [`enqueue_async`]. Use for
/// any diagnostic that fires from inside the Windows `WH_KEYBOARD_LL`
/// callback thread (rdev boundary trace, tracker `[chord]` trace) —
/// see the module-level "Off-callback async sink" section.
#[macro_export]
macro_rules! diag_log_async {
    ($($arg:tt)*) => {{
        $crate::diag::enqueue_async(format!($($arg)*));
    }};
}

pub use crate::diag_log_async as log_async;

/// Diagnostic log macro. Formats the arguments once and hands the
/// String to [`write_line`]. Use for any diagnostic that must be
/// visible after the fact on Windows GUI installs — the OS listener
/// startup path, the supervisor's Phase-B install / fallback branches,
/// the [`crate::hotkey::install_hotkey`] error surface.
///
/// Example:
/// ```ignore
/// crate::diag::log!("[hotkey] rdev listener failed: {msg}");
/// crate::diag::log!("[runtime] Phase B installed (driver={driver}, chord={chord})");
/// ```
#[macro_export]
macro_rules! diag_log {
    ($($arg:tt)*) => {{
        $crate::diag::write_line(&format!($($arg)*));
    }};
}

pub use crate::diag_log as log;
