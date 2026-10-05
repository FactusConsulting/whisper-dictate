//! Single process-wide tee sink and fallible output paths.

use super::config::LEVEL;
use super::LogLevel;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// The tee sink's type. A boxed `dyn Write` rather than a bare
/// `std::fs::File`.
///
/// Production only ever stores the rotating writer that
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
/// Successive GUI launches append until the 4 MiB limit, then keep one
/// bounded previous generation. Opening alone never prunes an old log.
/// The caller's session marker makes launch boundaries visible.
///
/// Re-install swaps the file: calling this twice with different paths
/// replaces the writer. This is what tests want (each test uses a temp
/// path); production callers install exactly once from
/// `whisper-dictate-gui::main`, so the swap is invisible there.
pub fn install_gui_diagnostic_log(path: &Path) -> std::io::Result<()> {
    let file = crate::log_rotation::RotatingLog::open(path)?;
    let slot = diag_file();
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(Box::new(file));
    }
    let _ = START.set(Instant::now());
    Ok(())
}

/// Write one diagnostic line: to the tee file (if installed) AND to
/// `eprintln!` (so the CLI binary's console stays informative). Each
/// line is prefixed with `t=<ms>` measured from the first log call so
/// timing between install / press / error events is inspectable.
///
/// Callers use the [`log!`] macro rather than this function directly
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
/// explicitly discarded via `let _ =`; do NOT reintroduce `unwrap()`
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
