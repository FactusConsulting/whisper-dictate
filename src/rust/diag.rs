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
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

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
            std::env::var_os("USERPROFILE").map(|home| {
                PathBuf::from(home)
                    .join("AppData")
                    .join("Local")
            })
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
pub fn write_line(message: &str) {
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
