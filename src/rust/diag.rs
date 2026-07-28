//! Diagnostic file sink for the Windows GUI binary — solves the
//! "stderr is silent" observation from Windows PTT bug reports.
//!
//! Context: `whisper-dictate-gui.exe` is built with
//! `windows_subsystem = "windows"` so a tray shortcut / autostart never
//! flashes a cmd window (see `whisper-dictate-gui.rs`). The tradeoff is
//! that the process has NO console attached, so `eprintln!` calls
//! (rdev listener startup errors, supervisor Phase-B fallback lines,
//! `[hotkey] ...` diagnostics) go to a discarded stderr handle and the
//! operator has zero signal when PTT silently misbehaves. The CLI
//! (`whisper-dictate.exe`) does not have this problem — it stays
//! console-subsystem and stderr flows to the launching shell.
//!
//! This module lets the GUI binary open a diagnostic file at startup
//! (typically `%LOCALAPPDATA%\WhisperDictate\gui-diagnostic.log`) and
//! then tee every diagnostic line into that file, so a future Windows
//! PTT wedge is inspectable after the fact without a rebuild.
//!
//! ## Contract
//!
//! * [`install_gui_diagnostic_log`] opens the file for append and stores
//!   it in a process-wide slot. Idempotent (repeat installs replace the
//!   previous file, first-writer discipline is not needed here — the
//!   only caller is `whisper-dictate-gui::main` at startup).
//! * [`log`] appends one line to the diagnostic file (if installed) and
//!   ALSO writes it to `eprintln!` (so the CLI binary, which never
//!   installs the file, still surfaces the same diagnostics via its
//!   console-attached stderr). Every line gets a monotonic `t=<ms>`
//!   prefix so the file is grep-friendly across a session.
//! * Nothing panics — on any I/O error the write is silently dropped
//!   (we're already on a diagnostic path; a secondary write failure
//!   changes nothing observable and blocking on it would defeat the
//!   purpose).
//!
//! ## Non-goals
//!
//! * NOT a general-purpose log framework. There's no level filter, no
//!   structured fields, no async batching. It's a fixed-format `tee`
//!   for the handful of `crate::diag::log!` call sites that gate
//!   Windows debuggability.
//! * NOT an fd-level stderr redirect. Redirecting fd 2 on Windows
//!   requires either `libc::freopen` (needs the C stderr FILE*, not
//!   trivially exposed by Rust's libc on MSVC) or `SetStdHandle` +
//!   `_dup2` (needs new deps or extern bindings). The call-site macro
//!   approach captures every log line we care about with less blast
//!   radius; unmodified `eprintln!` sites keep their existing behaviour
//!   (visible in CLI, discarded in GUI — same as today, just now with
//!   an explicit debug channel for the ones that matter).

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// Standard log level, read once at startup from [`LOG_ENV_VAR`] and
/// cached in a process-wide atomic so trace call sites can check with
/// a single relaxed load — matters because the Windows LL-hook thread
/// fires the check on every desktop-wide key press.
///
/// Names follow the Rust ecosystem convention (`log`/`tracing`) so a
/// user reading `VOICEPI_LOG=debug` recognises what to expect without
/// a per-project glossary:
///
/// * [`Off`] — no diagnostic output at all. Even startup markers are
///   suppressed.
/// * [`Error`] — only errors that stopped something working (rdev
///   listener startup failure, hotkey register failure, ...).
/// * [`Warn`] — errors we recovered from (fallback branch taken,
///   Phase-B degraded, ...).
/// * [`Info`] — normal lifecycle events: startup markers, the rdev
///   listener heartbeat and rate-limited per-event trace (shipped by
///   PR #646), session start / stop events. Default for release
///   binaries — keeps existing behaviour without adding per-key noise.
/// * [`Debug`] — internal state transitions: rdev callback boundary
///   trace, chord matcher trace, coordinator state transitions,
///   session dispatch. Bump to this level when investigating a wedge
///   the info-level heartbeat cannot pinpoint.
/// * [`Trace`] — everything, including the parallel Windows
///   `WH_KEYBOARD_LL` hook that dumps every desktop-wide key event's
///   virtual-key / scan code / flags. High volume (500+ lines/min of
///   typing); only for active debugging.
///
/// Unknown values default to [`Info`] (the release default) plus a
/// one-shot warning so a `VOICEPI_LOG=debgu` typo is visible in the
/// log rather than silently downgraded to `Off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// Numeric encoding for the [`LEVEL`] atomic. Ordered so a
    /// monotone comparison (`current >= threshold as u8`) works; call
    /// sites should still prefer the [`info_enabled`] /
    /// [`debug_enabled`] / [`trace_enabled`] helpers for readability.
    const fn as_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
            Self::Debug => 4,
            Self::Trace => 5,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Off,
            1 => Self::Error,
            2 => Self::Warn,
            4 => Self::Debug,
            5 => Self::Trace,
            _ => Self::Info,
        }
    }

    /// Parse a raw env-var value. Case-insensitive; whitespace
    /// trimmed. Returns `None` for unknown values so [`init_from_env`]
    /// can warn and pick the [`Info`] default rather than silently
    /// promoting a typo. Accepts the standard names PLUS the older
    /// numeric aliases the rest of the settings surface uses (`0`/`1`)
    /// so a user typing `VOICEPI_LOG=1` gets the same behaviour they
    /// know from `VOICEPI_DEBUG=1`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "no" | "none" => Some(Self::Off),
            "error" | "err" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            // Empty is treated the same as unset → the release default
            // (Info). Truthy synonyms map to Info too so an existing
            // habit of "VOICEPI_LOG=1" doesn't accidentally jump to
            // Debug or Trace and flood the tee file.
            "" | "info" | "1" | "true" | "yes" | "on" | "default" => Some(Self::Info),
            "debug" | "dbg" => Some(Self::Debug),
            "trace" | "verbose" | "all" | "full" => Some(Self::Trace),
            _ => None,
        }
    }

    /// Short lowercase name for the log-emitted startup line. Stable
    /// across releases so grep strings in support runbooks
    /// (`grep VOICEPI_LOG=debug`) keep working.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// Env-var name that selects the [`LogLevel`]. Mirrors `RUST_LOG` in
/// naming so a Rust developer intuits what it does without reading
/// the docs. Documented in `docs/CONFIGURATION.md` under the
/// "Diagnostic env vars" section.
pub const LOG_ENV_VAR: &str = "VOICEPI_LOG";

/// Process-wide cached level. Written once by [`init_from_env`], read
/// on every trace call site. Atomic (not `OnceLock<LogLevel>`) so the
/// check on the LL-hook hot path is a single relaxed load — a
/// `OnceLock::get()` would go through the `once_cell::race` guard,
/// which is heavier than we can afford in the raw-hook callback.
///
/// Default is [`LogLevel::Info`] before [`init_from_env`] runs so the
/// tiny handful of startup lines emitted BEFORE the env var is read
/// (`install_gui_diagnostic_log` completion) are still visible. The
/// moment `init_from_env` runs, the resolved value overwrites this.
static LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info.as_u8());

/// One-shot latch so [`init_from_env`] logging the "unknown value"
/// warning is idempotent — a second call from a test won't re-emit
/// the warning line.
static UNKNOWN_VALUE_WARNED: OnceLock<()> = OnceLock::new();

/// Read [`LOG_ENV_VAR`] from the environment and cache the resolved
/// [`LogLevel`] in the process-wide atomic. Called exactly once from
/// `whisper-dictate-gui::main` right after
/// [`install_gui_diagnostic_log`] so the startup marker line already
/// benefits from the picked level.
///
/// Returns the resolved level so the caller can include it in the
/// startup marker.
///
/// * Unset / empty → [`LogLevel::Info`] (release default).
/// * A known value → that level.
/// * An unknown value → [`LogLevel::Info`] plus a one-shot warning
///   emitted through [`log`] so the operator sees the typo.
pub fn init_from_env() -> LogLevel {
    let raw = std::env::var(LOG_ENV_VAR).unwrap_or_default();
    let resolved = match LogLevel::parse(&raw) {
        Some(level) => level,
        None => {
            if UNKNOWN_VALUE_WARNED.set(()).is_ok() {
                crate::diag::log!(
                    "[diag] unknown {LOG_ENV_VAR}={raw:?}; defaulting to `info`. \
                     Accepted values: off, error, warn, info, debug, trace."
                );
            }
            LogLevel::Info
        }
    };
    LEVEL.store(resolved.as_u8(), Ordering::Relaxed);
    resolved
}

/// Current log level. Cheap — one relaxed atomic load.
pub fn current_level() -> LogLevel {
    LogLevel::from_u8(LEVEL.load(Ordering::Relaxed))
}

/// True when the level is [`LogLevel::Info`] or more verbose. Gate
/// for normal lifecycle diagnostics: startup markers, the rdev
/// listener heartbeat, rate-limited per-event trace (PR #646),
/// session-start events. Kept ON by default for release binaries so
/// nothing changes for existing users.
#[inline]
pub fn info_enabled() -> bool {
    LEVEL.load(Ordering::Relaxed) >= LogLevel::Info.as_u8()
}

/// True when the level is [`LogLevel::Debug`] or more verbose. Gate
/// for internal-state trace: rdev callback boundary, chord matcher,
/// coordinator state transitions, session dispatch refuse/emit
/// branches. Off by default — users opt in with
/// `VOICEPI_LOG=debug` when investigating a wedge that info-level
/// alone can't pinpoint.
#[inline]
pub fn debug_enabled() -> bool {
    LEVEL.load(Ordering::Relaxed) >= LogLevel::Debug.as_u8()
}

/// True when the level is [`LogLevel::Trace`]. Gate for the
/// highest-volume layer: the parallel Windows `WH_KEYBOARD_LL` hook
/// that dumps every desktop-wide key event. Off by default; opt in
/// with `VOICEPI_LOG=trace` only when actively debugging a
/// key-drop where debug-level cannot see the event on either side.
#[inline]
pub fn trace_enabled() -> bool {
    LEVEL.load(Ordering::Relaxed) >= LogLevel::Trace.as_u8()
}

/// Test-only accessor to reset the cached level so a follow-up test
/// can drive [`init_from_env`] from a fresh state without leaking the
/// previous test's env-var choice.
#[cfg(test)]
pub(crate) fn reset_level_for_tests() {
    LEVEL.store(LogLevel::Info.as_u8(), Ordering::Relaxed);
}

/// Process-wide slot for the diagnostic file writer. `None` means "not
/// installed" (readers skip the file write). Uses `Mutex<Option<File>>`
/// rather than `OnceLock<Mutex<File>>` so re-installing swaps the file
/// (important for tests that install with a temp path and expect their
/// writes to land there rather than in a sibling test's leftover file).
/// Production callers install exactly once from
/// `whisper-dictate-gui::main`, so the swap semantics are invisible in
/// shipping code.
fn diag_file() -> &'static Mutex<Option<std::fs::File>> {
    static DIAG_FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
    DIAG_FILE.get_or_init(|| Mutex::new(None))
}

/// Monotonic clock reference set on the first log call (or by
/// [`install_gui_diagnostic_log`]). The `t=<ms>` prefix on every line
/// gives a session-relative timeline for grepping install → press →
/// error timing without needing to correlate wall-clock timestamps.
static START: OnceLock<Instant> = OnceLock::new();

/// Where the GUI should place its diagnostic log. Returns `None` on
/// non-Windows targets and when the OS did not expose `LOCALAPPDATA`
/// (an unusual configuration — we do not fall back to the working
/// directory because writing the log next to `whisper-dictate-gui.exe`
/// would fail on an installed layout under `C:\Program Files\`).
///
/// The path resolves to `<LOCALAPPDATA>\WhisperDictate\gui-diagnostic.log`,
/// mirroring the existing `%APPDATA%\WhisperDictate\` convention the
/// config layer uses — but placed in the LOCAL (per-machine, non-roaming)
/// AppData branch so this diagnostic never syncs with the user's
/// roaming profile.
#[cfg(windows)]
pub fn default_gui_diagnostic_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|home| PathBuf::from(home).join("AppData").join("Local"))
        })?;
    Some(base.join("WhisperDictate").join("gui-diagnostic.log"))
}

/// Non-Windows stub — the GUI diagnostic log is a Windows-only concern
/// (the Linux + macOS builds keep their console-attached stderr and
/// don't need the tee).
#[cfg(not(windows))]
pub fn default_gui_diagnostic_path() -> Option<PathBuf> {
    None
}

/// Install the diagnostic file at `path`. Creates the parent directory
/// if needed, opens the file for append, and stores it in the
/// process-wide slot so subsequent [`log`] calls tee there. Returns Err
/// with the underlying `io::Error` when the file cannot be opened; the
/// caller (`whisper-dictate-gui::main`) is expected to swallow that
/// error - a missing diagnostic must not stop the GUI from starting.
///
/// The file is opened in append mode so successive GUI launches
/// accumulate into the same file (with a session-marker line the caller
/// writes right after install so the append boundary is visible).
///
/// Re-install swaps the file: calling this twice with different paths
/// replaces the writer. This is what tests want (each test uses a temp
/// path); production callers install exactly once from
/// `whisper-dictate-gui::main`, so the swap is invisible there.
pub fn install_gui_diagnostic_log(path: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let slot = diag_file();
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(file);
    }
    let _ = START.set(Instant::now());
    Ok(())
}

/// Write one diagnostic line: to the tee file (if installed) AND to
/// `eprintln!` (so the CLI binary's console stays informative). Each
/// line is prefixed with `t=<ms>` measured from the first log call so
/// timing between install / press / error events is inspectable.
///
/// Callers use the [`log!`] macro rather than this function directly —
/// the macro forwards a `format_args!` result so the caller pays no
/// allocation when the diagnostic sink is not installed.
///
/// A resolved level of [`LogLevel::Off`] short-circuits before any
/// write happens — no stderr line, no tee-file write, no timer
/// initialisation. The docs promise "`off` — Nothing — not even
/// startup markers", so lifecycle call sites that predate a level
/// gate (the GUI startup marker, unconditional supervisor Phase-B
/// notes, ...) still respect `VOICEPI_LOG=off`. Codex P2 #651
/// discussion r3663372988.
pub fn write_line(message: &str) {
    if LEVEL.load(Ordering::Relaxed) == LogLevel::Off.as_u8() {
        // Suppress unconditionally: no stderr, no tee, no timer init.
        // Startup markers, error surfaces, and every other call site
        // funnels through here, so a single gate at the sink makes
        // `Off` an actual no-op without touching every caller.
        return;
    }
    let ms = START.get_or_init(Instant::now).elapsed().as_millis();
    let line = format!("t={ms}ms {message}");
    let stderr = std::io::stderr();
    write_line_to(&mut stderr.lock(), &line);
}

/// Sink half of [`write_line`], parameterised over the "stderr" writer
/// so a test can drive a genuinely failing one.
///
/// Writes `line` to `stderr_sink` and (independently) appends it to the
/// installed diagnostic file. NEITHER write may panic or short-circuit
/// the other: a closed / invalid stderr must still leave the tee-file
/// record intact, because that record is the whole point of the GUI
/// diagnostic path.
///
/// Fallible-write contract (Codex P1 #644 discussion r3658983548): a
/// plain `eprintln!` panics on `write_all` failure, and on Windows the
/// hidden-subsystem launcher / a consumer that closed a redirected pipe
/// can leave stderr in exactly that "closed / invalid" state. A panic
/// here would abort the unconditional GUI session marker at startup, or
/// kill the calling thread when a later diagnostic fires — losing the
/// very file record intended to diagnose the failure. So every `Err` is
/// explicitly discarded via `let _ =`; do NOT reintroduce `unwrap()` /
/// `expect()` / `eprintln!` on either side.
///
/// `pub(crate)` + parameterised purely so
/// `diag_tests::write_line_to_survives_a_failing_stderr_sink` can pass
/// a writer whose `write` always errors and assert both halves of the
/// contract directly, rather than only banning `eprintln!` textually
/// (Codex P2 #668 discussion 3666529224).
pub(crate) fn write_line_to<W: Write>(stderr_sink: &mut W, line: &str) {
    // Always stderr — CLI users get real-time output, GUI users on
    // non-installed builds still see whatever their console has.
    let _ = writeln!(stderr_sink, "{line}");
    let _ = stderr_sink.flush();
    if let Ok(mut guard) = diag_file().lock() {
        if let Some(file) = guard.as_mut() {
            // Best-effort - ignore write errors. A full disk or a
            // suddenly-unwritable AppData folder cannot silence the
            // stderr write above; both writes are attempted independently.
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }
}

// ---------------------------------------------------------------------------
// Off-callback async sink (Codex P1 #646 r3661145589 + #668 3665741341).
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
// rdev boundary trace only, but Codex P1 #668 3665741341 pointed out
// that the tracker's `[chord]` debug trace ALSO runs on the same
// callback thread (`dispatch_raw_event` -> `KeyTracker::handle` ->
// `crate::diag::log!`). Consolidating the queue in `crate::diag` means
// both call sites feed the same writer thread and any future callsite
// on the LL-hook path gets the same protection.
// ---------------------------------------------------------------------------

/// Bounded queue capacity for [`enqueue_async`]. 256 lines absorbs
/// realistic bursts (rate-limited by the callers) even with a stalled
/// AppData volume; a genuine flood dropping lines is preferable to a
/// wedged hook. Codex P1 #646 discussion r3661145589 + Codex P1 #668
/// discussion 3665741341.
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
/// now-measurable follow-up.
pub const ASYNC_QUEUE_CAPACITY: usize = 256;

/// Process-wide sender to the off-callback trace writer thread. `None`
/// until [`ensure_async_writer`] runs (from the first callback-path
/// caller); once installed it persists for the process lifetime because
/// the writer thread cannot be cleanly torn down (the OS listener that
/// feeds it is itself unjoinable).
static ASYNC_QUEUE_TX: OnceLock<SyncSender<AsyncRecord>> = OnceLock::new();

/// How long the writer parks on an empty queue before waking up to check
/// whether anything was shed after the last record it wrote.
///
/// A plain blocking `recv()` here is a correctness bug, not a style
/// choice (Codex P2 #680 comment 3667524121): the process-wide
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
/// Records carry the shed count that preceded them rather than the
/// writer reading a global counter at write time, so the coalesced
/// marker lands at the QUEUE POSITION of the gap instead of at whatever
/// position the writer happened to reach first (Codex P2 #680 comment
/// 3667524111). See [`enqueue_async_into`] for why that distinction
/// matters.
pub(crate) enum AsyncRecord {
    /// One trace line, plus the number of records shed immediately
    /// before this one was accepted (`0` on every healthy enqueue).
    Line { drops_before: u64, message: String },
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

/// Records [`enqueue_async`] has shed since the writer thread last
/// emitted a coalesced `[diag-async] dropped=` marker.
///
/// Before this existed, a full queue dropped the line and told nobody:
/// `gui-diagnostic.log` looked identical whether the LL-hook callback
/// was quiet or whether the queue had shed a burst, so a reader could
/// not distinguish "nothing happened" from "the sink was too slow to
/// record what happened" — on exactly the slow-AppData scenario the
/// queue exists for. Bumped with a single relaxed `fetch_add` on the
/// LL-hook thread (no allocation, no lock) and taken back to zero
/// either by the next ACCEPTED record — which carries the count to the
/// writer so the marker keeps its queue position, see
/// [`enqueue_async_into`] — or, when no such record ever arrives, by
/// the writer itself on its [`ASYNC_PARK_POLL`] wakeup.
static ASYNC_DROPPED: AtomicU64 = AtomicU64::new(0);

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
/// Codex P2 #680 comment 3668174780. The shed count travels with the
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
fn close_burst_with_pending_drops<F>(
    burst: &mut BurstState,
    dropped: &AtomicU64,
    capacity: usize,
    sink: &mut F,
) where
    F: FnMut(&str),
{
    burst.total += dropped.swap(0, Ordering::Relaxed);
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
    record: AsyncRecord,
    capacity: usize,
    pending: &AtomicUsize,
    burst: &mut BurstState,
    sink: &mut F,
) where
    F: FnMut(&str),
{
    let AsyncRecord::Line {
        drops_before,
        message,
    } = record;
    burst.observe_record(drops_before, capacity, sink);
    sink(&message);
    pending.fetch_sub(1, Ordering::Relaxed);
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
/// Loops until every sender is dropped, which never happens in a
/// shipping process: the `OnceLock` keeps a sender alive for the
/// process lifetime, matching the rdev listener's own lifetime model.
/// The trailing [`emit_pending_async_drops`] therefore only runs in
/// tests and on a future explicit-shutdown path.
///
/// ## Why the park is `recv_timeout` and not `recv`
///
/// Because the sender is immortal, a blocking `recv` parks FOREVER once
/// the queue drains. Records shed after the last surviving record have
/// no "next record" to ride on, so under a plain `recv` a burst that
/// ends the trace is reported only if the trace later resumes — i.e.
/// never, in the wedge case this whole mechanism exists to diagnose
/// (Codex P2 #680 comment 3667524121). Parking with
/// [`ASYNC_PARK_POLL`] turns "reported only if more events arrive" into
/// "reported within half a second, unconditionally"; the timeout arm is
/// the only place a marker can be emitted with no record to attach it
/// to, which is exactly the case that needs it.
///
/// ## Why the loop carries [`BurstState`]
///
/// A marker per dequeued record with a non-zero carried count is nearly
/// one marker per surviving record under a sustained overload, which
/// doubles the load on the sink that was already too slow (Codex P2 #680
/// comment 3668174780). The state machine collapses that to one notice
/// when the episode starts plus one summary when the queue catches up.
pub(crate) fn run_async_writer_loop<F>(
    rx: Receiver<AsyncRecord>,
    capacity: usize,
    pending: &AtomicUsize,
    dropped: &AtomicU64,
    mut sink: F,
) where
    F: FnMut(&str),
{
    let mut burst = BurstState::default();
    loop {
        match rx.recv_timeout(ASYNC_PARK_POLL) {
            Ok(record) => write_async_record(record, capacity, pending, &mut burst, &mut sink),
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

/// Idempotently install the off-callback trace writer thread. Safe to
/// call from every callback-path caller because the writer is gated by
/// [`OnceLock`]; only the first caller spawns the thread, every
/// subsequent caller no-ops.
pub fn ensure_async_writer() {
    ASYNC_QUEUE_TX.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<AsyncRecord>(ASYNC_QUEUE_CAPACITY);
        // Best-effort spawn — a thread-spawn failure here would still let
        // callers install (the callback simply won't tee its traces for
        // that process), so we swallow the Err rather than propagate.
        // Named for `taskkill /t` traces and future panic-hook attribution.
        let _ = thread::Builder::new()
            .name("vp-hotkey-diag-async".to_owned())
            .spawn(move || {
                run_async_writer_loop(
                    rx,
                    ASYNC_QUEUE_CAPACITY,
                    &ASYNC_PENDING,
                    &ASYNC_DROPPED,
                    write_line,
                );
            });
        tx
    });
}

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
        enqueue_async_into(tx, &ASYNC_PENDING, &ASYNC_DROPPED, message);
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
/// early (Codex P2 #680 comment 3667524111). Handing the outstanding
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
    pending: &AtomicUsize,
    dropped: &AtomicU64,
    message: String,
) {
    let drops_before = take_pending_drops(dropped);
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
            dropped.fetch_add(drops_before + 1, Ordering::Relaxed);
        }
        Err(TrySendError::Disconnected(_)) => {
            // The writer thread is gone. Not counted (and what we took
            // is deliberately not restored): the marker could never be
            // emitted, so the counter would only grow forever.
        }
    }
}

/// Take the outstanding shed count so it can be bound to a record.
///
/// The `load` fast path matters: this runs on the Windows LL-hook
/// callback thread for EVERY trace line, and the counter is zero in all
/// but the handful of enqueues that follow an actual gap.
fn take_pending_drops(dropped: &AtomicU64) -> u64 {
    if dropped.load(Ordering::Relaxed) == 0 {
        0
    } else {
        dropped.swap(0, Ordering::Relaxed)
    }
}

/// True once [`ensure_async_writer`] has populated the process-wide
/// sender. Callers on the LL-hook path never need this (they
/// fire-and-forget through [`enqueue_async`]); it exists so a
/// regression test can assert that a backend's install path actually
/// installed the writer, rather than silently dropping every queued
/// trace. Codex P2 #668 discussion 3666165045.
pub fn async_writer_installed() -> bool {
    ASYNC_QUEUE_TX.get().is_some()
}

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
    ASYNC_DROPPED.load(Ordering::Relaxed)
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
