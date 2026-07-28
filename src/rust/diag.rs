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
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Instant;

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
static ASYNC_QUEUE_TX: OnceLock<SyncSender<String>> = OnceLock::new();

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
/// LL-hook thread (no allocation, no lock) and `swap`ped back to zero
/// by the writer thread when it emits the marker.
static ASYNC_DROPPED: AtomicU64 = AtomicU64::new(0);

/// The coalesced load-shed marker the writer emits before the next
/// record it writes after one or more drops.
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

/// Emit the coalesced [`async_dropped_marker`] if anything was shed
/// since the last time this ran, then reset the counter.
///
/// `swap` rather than `load` + `store` so a drop racing in between the
/// read and the reset is carried into the NEXT marker instead of being
/// lost: the accounting is allowed to be late, never wrong.
///
/// One marker per burst, never one line per drop — a per-drop marker
/// would be unbounded write amplification against the very sink that
/// was too slow to keep up in the first place.
fn emit_pending_async_drops<F>(dropped: &AtomicU64, capacity: usize, sink: &mut F)
where
    F: FnMut(&str),
{
    let shed = dropped.swap(0, Ordering::Relaxed);
    if shed > 0 {
        sink(&async_dropped_marker(shed, capacity));
    }
}

/// The writer thread's whole body, parameterised over the receiver,
/// the two counters and the sink.
///
/// Production calls this from [`ensure_async_writer`] with the
/// process-wide statics and [`write_line`]; `diag_tests` calls it on a
/// scoped thread with a tiny channel and a sink it can stall on demand,
/// which is the only way to drive the "queue filled while the sink was
/// wedged" path deterministically — the production sink is a file write
/// no test can pause.
///
/// Loops until every sender is dropped, which never happens in a
/// shipping process: the `OnceLock` keeps a sender alive for the
/// process lifetime, matching the rdev listener's own lifetime model.
/// The trailing [`emit_pending_async_drops`] therefore only runs in
/// tests and on a future explicit-shutdown path, and exists so records
/// shed after the LAST surviving record are still accounted for (there
/// is no "next record" to prefix them onto).
pub(crate) fn run_async_writer_loop<F>(
    rx: Receiver<String>,
    capacity: usize,
    pending: &AtomicUsize,
    dropped: &AtomicU64,
    mut sink: F,
) where
    F: FnMut(&str),
{
    while let Ok(line) = rx.recv() {
        // Marker FIRST, so a reader of the log sees the gap announced
        // immediately before the first record that followed it.
        emit_pending_async_drops(dropped, capacity, &mut sink);
        sink(&line);
        // Decrement AFTER the write so a test polling
        // `ASYNC_PENDING == 0` (via `flush_async_for_tests`) sees the
        // file has actually been written to.
        pending.fetch_sub(1, Ordering::Relaxed);
    }
    emit_pending_async_drops(dropped, capacity, &mut sink);
}

/// Idempotently install the off-callback trace writer thread. Safe to
/// call from every callback-path caller because the writer is gated by
/// [`OnceLock`]; only the first caller spawns the thread, every
/// subsequent caller no-ops.
pub fn ensure_async_writer() {
    ASYNC_QUEUE_TX.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<String>(ASYNC_QUEUE_CAPACITY);
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
/// [`ASYNC_DROPPED`], and the writer thread announces the total as one
/// coalesced [`async_dropped_marker`] line before the next record it
/// writes. Without that, a shed burst and a quiet period are
/// indistinguishable in `gui-diagnostic.log`, which makes the trace
/// untrustworthy for exactly the slow-sink scenario the queue exists
/// for.
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
pub(crate) fn enqueue_async_into(
    tx: &SyncSender<String>,
    pending: &AtomicUsize,
    dropped: &AtomicU64,
    message: String,
) {
    match tx.try_send(message) {
        Ok(()) => {
            pending.fetch_add(1, Ordering::Relaxed);
        }
        Err(TrySendError::Full(_)) => {
            // Shed the newest record but ACCOUNT for it, so the writer
            // can tell the log reader the trace has a gap and how big.
            dropped.fetch_add(1, Ordering::Relaxed);
        }
        Err(TrySendError::Disconnected(_)) => {
            // The writer thread is gone. Not counted: the marker could
            // never be emitted, so the counter would only grow forever.
        }
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
