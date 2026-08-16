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
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Mutex, Once, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::diag_shutdown_gate::ShutdownGate;

/// Drop accounting for the off-callback queue. Lives in its own module
/// (pure, lock-free logic worth reading on its own) and is re-exported
/// here so every call site keeps saying `crate::diag::DropLedger`.
pub(crate) use crate::diag_drop_ledger::DropLedger;

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
/// * [`Info`] — normal lifecycle events: startup markers, the rdev listener
///   heartbeat, rate-limited per-event trace, and session start / stop events.
///   This is the default for release binaries.
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
    configure_level(&raw)
}

/// Apply an already-resolved native diagnostic level without consulting or
/// mutating the process environment.
pub(crate) fn configure_level(raw: &str) -> LogLevel {
    let resolved = match LogLevel::parse(raw) {
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
/// listener heartbeat, rate-limited per-event trace, and session-start events.
/// This level is enabled by default for release binaries.
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

/// True when ANY diagnostic that fires from inside an OS input callback
/// can actually be emitted at the current level.
///
/// Every callback-path call site is individually gated: the rdev boundary
/// trace and the tracker `[chord]` trace by [`debug_enabled`], the rdev
/// per-event sample by [`info_enabled`], the Windows raw-hook dump by
/// [`trace_enabled`]. The most permissive of those is [`info_enabled`],
/// so that IS the union — below `info` not one of them can fire and the
/// off-callback writer has no work at all.
///
/// ## Why this deserves a name
///
/// It is the precondition for treating a dead
/// [`async_writer_result`] as FATAL. The rdev listener refuses to install
/// its OS hook when the writer thread failed to spawn, on the reasoning
/// that hooking would leave the operator reading a `gui-diagnostic.log`
/// whose callback trace cannot exist. That reasoning only holds while
/// something would have been written: at `off` / `error` / `warn` the
/// failed writer is UNUSED, so aborting turns a working Rust-hotkey
/// install into a Python fallback over a diagnostic nobody asked for -
/// and at `off` without even a line saying why. Spelling the condition
/// out here keeps the rdev driver and
/// [`crate::hotkey::manager::win_raw_hook::install_gate`] (which already
/// short-circuits on its own `trace_enabled` gate) making the same
/// judgement for the same reason.
#[inline]
pub fn callback_diagnostics_enabled() -> bool {
    info_enabled()
}

/// Test-only accessor to reset the cached level so a later test
/// can drive [`init_from_env`] from a fresh state without leaking the
/// previous test's env-var choice.
#[cfg(test)]
pub(crate) fn reset_level_for_tests() {
    LEVEL.store(LogLevel::Info.as_u8(), Ordering::Relaxed);
}

/// Test-only: force the cached level, so a test can assert a gate's
/// behaviour across all six levels without going through the environment
/// (which is process-global and races other suites). Callers MUST hold
/// `crate::diag_test_lock::DIAG_WRITER_LOCK` and reset afterwards.
///
/// Its only caller (`rdev_driver_tests::…`) lives behind `rust-hotkeys`,
/// so on a feature set that omits it — the devcontainer dev-loop runs
/// `--features ui-egui-glow` alone — this compiles with no callers and
/// trips `-D dead-code`. The seam is still wanted there: a future
/// non-hotkey test asserting a level gate should reach for this rather
/// than mutating `VOICEPI_LOG` process-globally.
#[cfg(test)]
#[cfg_attr(not(feature = "rust-hotkeys"), allow(dead_code))]
pub(crate) fn set_level_for_tests(level: LogLevel) {
    LEVEL.store(level.as_u8(), Ordering::Relaxed);
}

/// The tee sink's type. A boxed `dyn Write` rather than a bare
/// `std::fs::File`.
///
/// Production only ever stores the `File` that
/// [`install_gui_diagnostic_log`] opened, and the extra indirection is
/// invisible next to the `writeln!` + `flush` syscalls it guards. The
/// box buys the one thing a concrete `File` cannot give a test: a tee
/// whose mutex is FREE but whose `write` BLOCKS. That combination is
/// the whole of this contract — `try_lock` bounds
/// lock acquisition, not the file I/O behind it — and no temp file can
/// be made to stall on demand.
type TeeSink = Box<dyn Write + Send>;

/// Process-wide slot for the diagnostic file writer. `None` means "not
/// installed" (readers skip the file write). Uses `Mutex<Option<..>>`
/// rather than `OnceLock<Mutex<..>>` so re-installing swaps the file
/// (important for tests that install with a temp path and expect their
/// writes to land there rather than in a sibling test's leftover file).
/// Production callers install exactly once from
/// `whisper-dictate-gui::main`, so the swap semantics are invisible in
/// shipping code.
fn diag_file() -> &'static Mutex<Option<TeeSink>> {
    static DIAG_FILE: OnceLock<Mutex<Option<TeeSink>>> = OnceLock::new();
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
        *guard = Some(Box::new(file));
    }
    let _ = START.set(Instant::now());
    Ok(())
}

/// Install one process-wide panic hook for the GUI binary.
///
/// A Rust panic normally reaches the hidden GUI process's discarded stderr,
/// which makes an unexpected window exit look like a silent crash. Record the
/// payload, thread name, and source location in the existing diagnostic sink
/// first, then
/// delegate to the hook that was active before startup.
pub fn install_gui_panic_hook() {
    static GUI_PANIC_HOOK: Once = Once::new();
    GUI_PANIC_HOOK.call_once(|| {
        // Set up the dedicated panic writer before publishing the hook.
        // Panic records must not compete with high-rate callback tracing.
        ensure_panic_writer();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let location = info
                .location()
                .map(|location| (location.file(), location.line(), location.column()));
            // A panic must not wait for the tee mutex OR synchronously touch
            // a potentially wedged diagnostic volume. The dedicated,
            // unbounded channel cannot be filled by ordinary trace traffic;
            // its writer owns file I/O.
            enqueue_panic_report(format_panic_report(info.payload(), location));
            previous(info);
        }));
    });
}

/// Dedicated sender for panic records. This is intentionally separate from
/// the bounded callback-trace queue: preserving the one record that explains
/// a crash matters more than applying trace backpressure to it.
static PANIC_QUEUE_TX: OnceLock<mpsc::Sender<String>> = OnceLock::new();

fn ensure_panic_writer() {
    PANIC_QUEUE_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<String>();
        let _ = thread::Builder::new()
            .name("vp-panic-diag".to_owned())
            .spawn(move || {
                while let Ok(message) = rx.recv() {
                    write_line(&message);
                }
            });
        tx
    });
}

/// Offer a crash record without taking the tee mutex or performing file I/O
/// on the panicking thread. The channel is independent of regular trace
/// traffic so a saturated callback queue cannot discard a panic report.
fn enqueue_panic_report(message: String) {
    ensure_panic_writer();
    if let Some(tx) = PANIC_QUEUE_TX.get() {
        let _ = tx.send(message);
    }
}

pub(crate) fn format_panic_report(
    payload: &(dyn std::any::Any + Send),
    location: Option<(&str, u32, u32)>,
) -> String {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    let message = escape_panic_field(message);
    let thread = escape_panic_field(std::thread::current().name().unwrap_or("unnamed"));
    match location {
        Some((file, line, column)) => {
            format!("[panic] Rust panic thread={thread} at {file}:{line}:{column}: {message}")
        }
        None => format!("[panic] Rust panic thread={thread}: {message}"),
    }
}

/// Keep a panic report as one physical diagnostic record. Panic payloads are
/// allowed to contain newlines (for example assertion messages), but an
/// unescaped newline would make continuation text look like a separate log
/// record without its timestamp and category.
fn escape_panic_field(value: &str) -> String {
    value.replace('\r', "\\r").replace('\n', "\\n")
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
/// notes, ...) still respect `VOICEPI_LOG=off`.
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
    // The guard is handed over BY VALUE, not by `&mut`: `write_line_to`
    // drops it before it touches the tee mutex. See its docs for why
    // that ordering is load-bearing.
    write_line_to(stderr.lock(), &line);
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
/// Fallible-write contract: a
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
/// The test also verifies that a failing stderr sink does not prevent the
/// diagnostic file write.
///
/// ## Lock ordering: the stderr sink is taken BY VALUE and dropped first
///
/// This runs on the async writer
/// thread, and the tee lock below is the one that a wedged AppData
/// volume holds for an unbounded time. Production passes
/// `std::io::stderr().lock()` here, so as long as this function held
/// that guard across the tee write, a wedged writer pinned the PROCESS
/// stderr lock too — and [`write_line_nonblocking`], whose whole job is
/// to log past exactly such a wedge inside
/// [`crate::entrypoint::DIAG_DRAIN_DEADLINE`], blocked on
/// `stderr.lock()` and never reached its `try_lock`. The 500 ms exit
/// budget then bought nothing: teardown hung on the stderr lock instead
/// of on the tee mutex.
///
/// Taking `W` by value (rather than `&mut W`) is what makes the fix
/// enforceable: the explicit `drop` below releases the caller's guard,
/// and a regression that reintroduced `&mut W` could not compile the
/// `drop` at all. Do NOT reorder the tee write above it.
pub(crate) fn write_line_to<W: Write>(mut stderr_sink: W, line: &str) {
    // Always stderr — CLI users get real-time output, GUI users on
    // non-installed builds still see whatever their console has.
    let _ = writeln!(stderr_sink, "{line}");
    let _ = stderr_sink.flush();
    // Release the stderr guard BEFORE the potentially wedged tee write.
    drop(stderr_sink);
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

/// Non-blocking variant of [`write_line`] for call sites that must
/// never WAIT on the tee-file mutex but still want the record in the
/// file when the mutex happens to be free.
///
/// The exit
/// drain's timeout warning, on the reasoning that a blocking `log!`
/// there would queue behind a writer parked inside [`write_line_to`]
/// holding this very mutex.
///
/// ## NOT the exit-teardown warning sink any more
///
/// `try_lock` bounds the LOCK, not
/// the file I/O behind it, so on a FREE mutex this still performs a
/// synchronous `writeln!` + `flush` on the very volume that just failed
/// to drain. That path now uses [`write_line_stderr_only`], which
/// touches no tee state at all. Do not wire a teardown / deadline path
/// back to this function.
///
/// Returns `true` when the tee-file write was attempted, `false` when
/// the line went to stderr only (mutex contended or poisoned).
pub fn write_line_nonblocking(message: &str) -> bool {
    if LEVEL.load(Ordering::Relaxed) == LogLevel::Off.as_u8() {
        return false;
    }
    let ms = START.get_or_init(Instant::now).elapsed().as_millis();
    let line = format!("t={ms}ms {message}");
    let stderr = std::io::stderr();
    write_line_to_nonblocking(stderr.lock(), &line)
}

/// Sink half of [`write_line_nonblocking`], parameterised over the
/// "stderr" writer exactly as [`write_line_to`] is, so the companion
/// test can assert BOTH halves of the contract (stderr still gets the
/// line; the tee write is skipped rather than waited on) while holding
/// the tee mutex from the test thread.
///
/// `try_lock` (never `lock`) is the whole point of this function - do
/// not "simplify" it back to a blocking lock.
///
/// The stderr sink is taken by value and dropped before the tee attempt
/// for the same reason [`write_line_to`] does it: this is a teardown
/// path, and holding the process stderr lock across ANY tee interaction
/// is what let a wedged sink stall an unrelated logger.
pub(crate) fn write_line_to_nonblocking<W: Write>(mut stderr_sink: W, line: &str) -> bool {
    let _ = writeln!(stderr_sink, "{line}");
    let _ = stderr_sink.flush();
    drop(stderr_sink);
    match diag_file().try_lock() {
        Ok(mut guard) => {
            if let Some(file) = guard.as_mut() {
                let _ = writeln!(file, "{line}");
                let _ = file.flush();
            }
            true
        }
        // Contended (a wedged writer holds it) or poisoned - drop the
        // tee write rather than block. stderr already has the line.
        Err(_) => false,
    }
}

/// Emit one diagnostic line to stderr and to **nothing else**.
///
/// ## Why a third sink
///
/// This is the sink [`crate::entrypoint::drain_diagnostics_on_exit`]
/// warns through when [`drain_and_shutdown`] misses
/// [`crate::entrypoint::DIAG_DRAIN_DEADLINE`]. That warning is *about* a
/// tee sink that has already proven itself unresponsive, so it must not
/// go anywhere near it.
///
/// [`write_line_nonblocking`] is NOT good enough for that job, which is
/// the correction this function exists to make. Its `try_lock` bounds
/// only the LOCK ACQUISITION. When the drain fails while the tee mutex
/// happens to be free — the writer thread disconnected, or it released
/// the mutex a microsecond before the warning ran — the `try_lock`
/// SUCCEEDS and the warning then performs a synchronous `writeln!` +
/// `flush` on the same stalled AppData volume that wedged the writer in
/// the first place. Process exit blocks there indefinitely, past the
/// 500 ms deadline, inside the warning about the wedged sink.
///
/// A timeout warning has nothing to gain from the tee anyway: the whole
/// message is "the tee file may be short of records", so the operator
/// reading that file is not the audience. Stderr is.
///
/// The [`LogLevel::Off`] gate is kept for the same reason
/// [`write_line`] has one: `off` promises no output at all, including
/// from teardown paths.
pub fn write_line_stderr_only(message: &str) {
    if LEVEL.load(Ordering::Relaxed) == LogLevel::Off.as_u8() {
        return;
    }
    let ms = START.get_or_init(Instant::now).elapsed().as_millis();
    let line = format!("t={ms}ms {message}");
    let stderr = std::io::stderr();
    write_line_to_stderr_only(stderr.lock(), &line);
}

/// Sink half of [`write_line_stderr_only`], parameterised over the
/// writer exactly as its two siblings are.
///
/// The body must stay exactly this: one `writeln!`, one `flush`, and no
/// reference to [`diag_file`] in any form (not `lock`, not `try_lock`).
/// Adding one back reintroduces the blocking path — see
/// [`write_line_stderr_only`].
pub(crate) fn write_line_to_stderr_only<W: Write>(mut stderr_sink: W, line: &str) {
    let _ = writeln!(stderr_sink, "{line}");
    let _ = stderr_sink.flush();
}

/// Test-only handle on the tee-file mutex so the companion test can
/// hold it across a [`write_line_nonblocking`] call and prove the
/// non-blocking contract against the real mutex rather than a mock.
#[cfg(test)]
pub(crate) fn tee_mutex_for_tests() -> &'static Mutex<Option<TeeSink>> {
    diag_file()
}

/// Test-only: put an arbitrary writer in the process-wide tee slot.
///
/// [`install_gui_diagnostic_log`] can only install a real file, and a
/// real file cannot be made to stall on demand. This is how
/// `diag_tests::the_exit_timeout_warning_does_not_write_to_a_free_but_blocked_tee`
/// builds the exact shape required here: a
/// tee whose mutex is acquirable and whose `write` never returns.
///
/// Callers MUST hold `crate::diag_test_lock::DIAG_WRITER_LOCK` and put
/// the slot back (`None`, or a fresh install) before releasing it.
#[cfg(test)]
pub(crate) fn install_tee_sink_for_tests(sink: Option<TeeSink>) {
    if let Ok(mut guard) = diag_file().lock() {
        *guard = sink;
    }
}

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

/// Recorded reason the off-callback writer thread never came up, or
/// unset when it did. Written exactly once, from inside
/// [`ensure_async_writer`]'s `OnceLock` initialiser, so it is settled by
/// the time that call returns for ANY thread.
///
/// Before this existed, [`ensure_async_writer`] swallowed the
/// `thread::Builder::spawn` `Err` outright: the sender was installed
/// regardless, so `async_writer_installed()` reported `true`, every
/// `enqueue_async` call filled a queue nobody was reading, and once the
/// 256 slots were gone every callback-path diagnostic - the rdev
/// boundary trace, the tracker `[chord]` trace, the Windows raw-hook
/// trace - vanished with no line anywhere saying why. A process in that
/// state looks exactly like a healthy quiet one in `gui-diagnostic.log`.
static ASYNC_WRITER_SPAWN_ERROR: OnceLock<String> = OnceLock::new();

/// Render `text` as printable ASCII on ONE line, escaping everything
/// else as `\u{...}`.
///
/// ## Why an OS error cannot be interpolated raw
///
/// `console_ascii_tests` is a scan of
/// source LITERALS: it proves the prose we wrote is ASCII, and it is
/// blind to whatever `{err}` expands to at runtime. A
/// [`std::io::Error`] from `thread::Builder::spawn` is OS-derived, and
/// `FormatMessageW` returns the system-locale text - on a Danish,
/// German, Japanese or Russian Windows the rendered message carries
/// non-ASCII. That string then reaches stderr through [`write_line`],
/// where a cmd.exe on a legacy code page renders it as mojibake, so the
/// one line explaining why the diagnostic pipeline is dead is itself
/// unreadable. An embedded newline would be worse still: it would split
/// one record into two and break the one-line-per-record grep contract
/// the whole tee file is read with.
///
/// Escaping rather than dropping: the escape is lossless and
/// round-trippable, so a support thread can still recover the original
/// localized text from the log when it matters, while the bytes that
/// actually reach the console stay in `0x20..=0x7e`.
pub(crate) fn ascii_escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        // Printable ASCII passes through untouched; DEL, the C0
        // controls (newline and tab included) and every non-ASCII
        // scalar become an escape.
        if c.is_ascii_graphic() || c == ' ' {
            out.push(c);
        } else {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        }
    }
    out
}

/// The message recorded (and logged) when the off-callback writer
/// thread fails to spawn.
///
/// A named function rather than an inline `format!` so the regression
/// test can assert the shape without duplicating the format string.
///
/// ASCII only: this string reaches stderr via [`write_line`] and
/// typographic punctuation renders as mojibake under cmd.exe on a
/// legacy code page (AGENTS.md; pinned by `console_ascii_tests`). The
/// prose here is covered by that source scan; the OS-derived `err` is
/// NOT (it does not exist until runtime), so it goes through
/// [`ascii_escaped`] - see that function for why.
pub(crate) fn writer_spawn_failure_message(err: &std::io::Error) -> String {
    let err = ascii_escaped(&err.to_string());
    format!(
        "[diag-async] writer thread spawn failed: {err} - callback-path \
         diagnostics (rdev boundary trace, chord trace, raw-hook trace) \
         cannot be written for the rest of this process"
    )
}

/// Record the writer thread's spawn outcome into `slot`.
///
/// `Ok` leaves the slot untouched (the absence of a recorded message IS
/// "the writer is running"); `Err` stores the formatted
/// [`writer_spawn_failure_message`]. Parameterised over the slot and
/// generic over the spawn payload so `diag_tests` can drive both arms
/// against a local `OnceLock` — a real `thread::Builder::spawn` failure
/// cannot be provoked in a unit test.
pub(crate) fn record_writer_spawn_outcome<T>(
    slot: &OnceLock<String>,
    spawned: std::io::Result<T>,
) -> Result<(), String> {
    match spawned {
        Ok(_) => Ok(()),
        Err(err) => {
            let msg = writer_spawn_failure_message(&err);
            let _ = slot.set(msg.clone());
            Err(msg)
        }
    }
}

/// Idempotently install the off-callback trace writer thread. Safe to
/// call from every callback-path caller because the writer is gated by
/// [`OnceLock`]; only the first caller spawns the thread, every
/// subsequent caller no-ops.
///
/// A spawn failure is NOT swallowed any more: it is recorded in
/// [`ASYNC_WRITER_SPAWN_ERROR`] and surfaced by
/// [`async_writer_result`], which the rdev listener consults before it
/// announces readiness.
pub fn ensure_async_writer() {
    ASYNC_QUEUE_TX.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<AsyncRecord>(ASYNC_QUEUE_CAPACITY);
        // Named for `taskkill /t` traces and future panic-hook attribution.
        let spawned = thread::Builder::new()
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
        // Still install the sender on failure (callers must keep working
        // and `enqueue_async` degrades to a silent drop), but REMEMBER
        // why, so the listener can report a dead diagnostic pipeline
        // instead of the process discovering it as missing log lines
        // hours later.
        if let Err(msg) = record_writer_spawn_outcome(&ASYNC_WRITER_SPAWN_ERROR, spawned) {
            write_line(&msg);
        }
        tx
    });
}

/// Install the off-callback writer (idempotently, via
/// [`ensure_async_writer`]) and report whether it is actually running.
///
/// `Ok(())` means a writer thread is draining the queue. `Err(msg)`
/// means the thread never spawned and every callback-path diagnostic
/// will be dropped for the rest of the process — the rdev listener maps
/// that to `SpawnError::WriterStartup` so `install_hotkey()` fails loudly
/// rather than running blind.
pub fn async_writer_result() -> Result<(), String> {
    ensure_async_writer();
    match ASYNC_WRITER_SPAWN_ERROR.get() {
        Some(msg) => Err(msg.clone()),
        None => Ok(()),
    }
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
/// unbound counter and before the record is offered to the channel —
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

/// True once [`ensure_async_writer`] has populated the process-wide
/// sender. Callers on the LL-hook path never need this (they
/// fire-and-forget through [`enqueue_async`]); it exists so a
/// regression test can assert that a backend's install path actually
/// installed the writer, rather than silently dropping every queued
/// trace.
pub fn async_writer_installed() -> bool {
    ASYNC_QUEUE_TX.get().is_some()
}

/// How long [`drain_and_shutdown`] naps between `try_send` attempts
/// while the bounded queue is full. Short enough that a queue draining
/// normally costs no measurable teardown latency, long enough that a
/// genuinely wedged writer is not spun on for the whole deadline.
const ASYNC_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Flush everything still queued for the off-callback writer and stop
/// the writer thread, waiting at most `deadline`.
///
/// ## Why this exists
///
/// The writer is a background thread. A `fn main` that simply returns
/// takes the process down with whatever is still in the queue, so the
/// records closest to the moment of interest - the tail of a PTT wedge
/// repro, the last chord trace before a crash-adjacent exit - are
/// exactly the ones a support thread never gets to read. Both shipping
/// binaries hit this: the GUI on tray exit, and the CLI on its finite
/// rdev-driven verbs (`self-test hotkey-boot`, `hotkey capture
/// --for-secs ...`) which install the same LL hook, feed the same
/// queue, and then return normally.
///
/// ## How
///
/// Pushes an [`AsyncRecord::Shutdown`] sentinel through the SAME
/// bounded queue as the records, so every record enqueued before the
/// call is ordered ahead of it; the writer flushes them, emits any
/// outstanding drop marker, acks and exits.
///
/// Returns `true` ONLY when the writer acknowledged within `deadline`,
/// or when no writer was ever installed (nothing was ever queued, so
/// nothing can be lost). Everything else is `false`: a timeout, and -
/// since a receiver that had
/// already disappeared, which means the thread never started (
/// [`ensure_async_writer`] swallows spawn errors) or panicked, and took
/// the queue with it unacknowledged. The bool is what lets the caller
/// warn the operator that the tee file may be short of records;
/// production must NOT treat it as fatal.
pub fn drain_and_shutdown(deadline: Duration) -> bool {
    match ASYNC_QUEUE_TX.get() {
        Some(tx) => drain_and_shutdown_into(tx, &ASYNC_GATE, deadline),
        // Never installed: no writer thread, no queue, nothing lost.
        None => true,
    }
}

/// Sender half of [`drain_and_shutdown`], parameterised over the
/// channel and the gate so `diag_tests` can drive every outcome (clean
/// flush, wedged-past-deadline, saturated producer) against a scoped
/// writer instead of the process-wide `OnceLock`, which no test can
/// reset.
///
/// Delegates to [`drain_and_shutdown_into_after`] with an empty seam, so
/// the function the companion tests drive IS the production drain rather
/// than a parallel copy.
pub(crate) fn drain_and_shutdown_into(
    tx: &SyncSender<AsyncRecord>,
    gate: &ShutdownGate,
    deadline: Duration,
) -> bool {
    drain_and_shutdown_into_after(tx, gate, deadline, || {})
}

/// [`drain_and_shutdown_into`] with a seam that runs immediately before
/// each `try_send` attempt.
///
/// `before_attempt` is where `diag_tests` reproduces the exact
/// interleaving the test reproduces - the writer frees
/// one slot and the still-firing callback producer refills it - without a
/// sleep or a scheduling race. Production passes an empty closure.
///
/// ## The gate closes BEFORE the first attempt
///
/// The queue is BOUNDED, so a plain `send` here could park forever behind
/// a stalled writer with a full queue - exactly the teardown hang the
/// deadline exists to prevent. `SyncSender::send_timeout` is still
/// unstable, so poll `try_send` (which hands the message back on `Full`)
/// inside the same overall budget.
///
/// Polling alone is not enough, and that is what the gate fixes. Between
/// two attempts this thread sleeps [`ASYNC_DRAIN_POLL_INTERVAL`], while
/// the producer is an unjoinable OS-callback thread offering a record
/// roughly every millisecond under the documented `VOICEPI_LOG=debug`
/// mouse trace. Every slot a slow-but-functional writer frees therefore
/// goes back to the producer before this thread wakes up, and the
/// sentinel can be starved for the entire deadline - the process exits
/// with the pre-request backlog undrained and warns the operator about a
/// writer that was never wedged.
///
/// Closing the gate first makes the queue drain-only: from that point the
/// writer removes records and nobody adds any, so the first slot it frees
/// is necessarily the sentinel's. It is a one-way latch and teardown may
/// run twice on a nested error path, so the close is idempotent.
pub(crate) fn drain_and_shutdown_into_after<H>(
    tx: &SyncSender<AsyncRecord>,
    gate: &ShutdownGate,
    deadline: Duration,
    mut before_attempt: H,
) -> bool
where
    H: FnMut(),
{
    // Stop admitting new lines BEFORE polling for sentinel space.
    gate.close();
    let started = Instant::now();
    let (ack_tx, ack_rx) = mpsc::channel::<()>();
    let mut sentinel = AsyncRecord::Shutdown(ack_tx);
    loop {
        before_attempt();
        match tx.try_send(sentinel) {
            Ok(()) => break,
            // The receiver is gone and our sentinel never reached a
            // writer, so nothing acknowledged the drain and whatever
            // was queued died with the thread. Reachable in production:
            // `ensure_async_writer` deliberately swallows a thread-spawn
            // error, and a panicking writer drops its receiver the same
            // way. Reporting success here would suppress the exit
            // warning and contradict this function's documented
            // contract on exactly the runs where diagnostics WERE lost
            // The receiver can disappear after the writer thread exits.
            //
            // There is no "already acknowledged, then disconnected"
            // case to rescue: an ack can only arrive on the `ack_rx`
            // below, which is reached only after a sentinel was
            // accepted. Every `Disconnected` on this send is therefore
            // an unexpected one.
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(returned)) => {
                let remaining = deadline.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    // The queue stayed full for the whole budget: the
                    // writer is wedged and the caller (a `main` on its
                    // way out) must not wait any longer.
                    return false;
                }
                sentinel = returned;
                thread::sleep(ASYNC_DRAIN_POLL_INTERVAL.min(remaining));
            }
        }
    }
    let remaining = deadline.saturating_sub(started.elapsed());
    // `is_ok()` folds BOTH failure shapes into `false` on purpose: a
    // `Timeout` (the writer is wedged) and a `Disconnected` (the writer
    // dropped the ack sender without answering - it panicked mid-drain)
    // are equally "the tee file may be short of records", which is the
    // only thing the caller does with this bool.
    ack_rx.recv_timeout(remaining).is_ok()
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
