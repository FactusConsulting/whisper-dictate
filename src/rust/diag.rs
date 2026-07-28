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
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
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
    // Always stderr — CLI users get real-time output, GUI users on
    // non-installed builds still see whatever their console has.
    eprintln!("{line}");
    if let Ok(mut guard) = diag_file().lock() {
        if let Some(file) = guard.as_mut() {
            // Best-effort - ignore write errors. A full disk or a
            // suddenly-unwritable AppData folder cannot silence the
            // eprintln! above; both writes are attempted independently.
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }
}

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
