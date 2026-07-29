//! Companion tests for [`crate::diag`]. Extracted from inline
//! `#[cfg(test)] mod tests` in `diag.rs` so the regression-test
//! discipline scanner (per AGENTS.md, `enforce-regression-test-discipline`)
//! sees a matching test file next to the production module.

#![cfg(test)]

use crate::diag::DropLedger;
use crate::diag::{
    current_level, debug_enabled, default_gui_diagnostic_path, info_enabled, init_from_env,
    install_gui_diagnostic_log, reset_level_for_tests, trace_enabled, LogLevel, LOG_ENV_VAR,
};
use crate::diag_shutdown_gate::ShutdownGate;
use crate::diag_test_lock::DIAG_WRITER_LOCK;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};

/// Serialise diag-mutation tests so parallel runs don't race the
/// process-wide writer slot. Consolidated onto the crate-wide
/// [`DIAG_WRITER_LOCK`] in `crate::diag_test_lock` (Codex P2 #665
/// discussion PRRT_kwDOSfNjQs6UYDJB): the previous function-local
/// `OnceLock<Mutex<()>>` was a different mutex from the identically
/// named lock in `hotkey::manager::tracker_tests`, so tests in the
/// two modules could still race the writer install even though each
/// suite serialised internally.
fn diag_test_lock() -> MutexGuard<'static, ()> {
    DIAG_WRITER_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// [`default_gui_diagnostic_path`] must place the file under the
/// WhisperDictate/ sub-directory of the Local AppData branch on
/// Windows, honouring `LOCALAPPDATA`. This pins the location the
/// user is told to inspect in support-thread docs — a rename here
/// makes the docs stale.
#[cfg(windows)]
#[test]
fn default_gui_diagnostic_path_uses_local_appdata_whisperdictate_folder() {
    let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let prev_local = std::env::var_os("LOCALAPPDATA");
    let prev_home = std::env::var_os("USERPROFILE");

    std::env::set_var("LOCALAPPDATA", r"C:\Users\test\AppData\Local");
    let path = default_gui_diagnostic_path().expect("LOCALAPPDATA path");
    let display = path.display().to_string();
    assert!(
        display.ends_with(r"WhisperDictate\gui-diagnostic.log"),
        "expected path under WhisperDictate/, got {display}",
    );
    assert!(
        display.contains(r"AppData\Local"),
        "expected path under AppData\\Local (LOCALAPPDATA, not the roaming APPDATA the config file uses), got {display}",
    );

    // USERPROFILE fallback when LOCALAPPDATA is missing.
    std::env::remove_var("LOCALAPPDATA");
    std::env::set_var("USERPROFILE", r"C:\Users\test");
    let path = default_gui_diagnostic_path().expect("USERPROFILE fallback path");
    let display = path.display().to_string();
    assert!(
        display.contains(r"AppData\Local\WhisperDictate"),
        "USERPROFILE fallback must synthesise AppData\\Local\\WhisperDictate, got {display}",
    );

    match prev_local {
        Some(v) => std::env::set_var("LOCALAPPDATA", v),
        None => std::env::remove_var("LOCALAPPDATA"),
    }
    match prev_home {
        Some(v) => std::env::set_var("USERPROFILE", v),
        None => std::env::remove_var("USERPROFILE"),
    }
}

/// On non-Windows targets the GUI diagnostic path is intentionally
/// `None` — Linux + macOS builds keep console-attached stderr and
/// don't need the tee. Pins the contract so a future well-meaning
/// refactor that always returns Some doesn't accidentally spam a
/// diagnostic file into every user's home directory.
#[cfg(not(windows))]
#[test]
fn default_gui_diagnostic_path_is_none_on_non_windows() {
    assert!(
        default_gui_diagnostic_path().is_none(),
        "non-Windows targets must return None (Linux/macOS keep console stderr)",
    );
}

/// `log!` must always emit to stderr AND (when the tee file is
/// installed) to that file, without panicking if the file cannot be
/// opened. The stderr side is unobservable from a unit test, but
/// the tee-file side is — write to a tempdir and assert the line
/// lands with the `t=<ms>` prefix.
#[test]
fn log_macro_writes_prefixed_line_to_installed_tee_file() {
    use std::io::Read;
    let _lock = diag_test_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("diag.log");

    // Install directly (bypassing the platform-picker) so the test
    // hits the tee-write path even on non-Windows targets.
    install_gui_diagnostic_log(&path).expect("install diag");

    crate::diag::log!("[test] first line {msg}", msg = "hello");
    crate::diag::log!("[test] second line");

    let mut contents = String::new();
    std::fs::File::open(&path)
        .expect("open diag file")
        .read_to_string(&mut contents)
        .expect("read diag file");
    assert!(
        contents.contains("[test] first line hello"),
        "diagnostic file must contain the formatted message: {contents:?}",
    );
    assert!(
        contents.contains("[test] second line"),
        "diagnostic file must contain the second line: {contents:?}",
    );
    assert!(
        contents.contains("t=") && contents.contains("ms "),
        "each line must carry a t=<ms> monotonic prefix: {contents:?}",
    );
}

// ---------------------------------------------------------------------
// VOICEPI_LOG level parsing and env-init.
//
// The level gate is the diagnostic-only PR's centre — every trace
// call site the F9-drop investigation added is behind either
// `info_enabled`, `debug_enabled`, or `trace_enabled`. These tests
// pin the parser's contract so a future tweak that (say) accepted
// "verbose" as an alias for `debug` instead of `trace`, or that
// promoted an unknown value to `Trace` on default, would have to fail
// a test before it could land — because those changes would silently
// break existing user-facing docs / support runbooks.
// ---------------------------------------------------------------------

#[test]
fn log_level_parse_accepts_standard_names() {
    assert_eq!(LogLevel::parse("off"), Some(LogLevel::Off));
    assert_eq!(LogLevel::parse("error"), Some(LogLevel::Error));
    assert_eq!(LogLevel::parse("warn"), Some(LogLevel::Warn));
    assert_eq!(LogLevel::parse("info"), Some(LogLevel::Info));
    assert_eq!(LogLevel::parse("debug"), Some(LogLevel::Debug));
    assert_eq!(LogLevel::parse("trace"), Some(LogLevel::Trace));
}

#[test]
fn log_level_parse_is_case_insensitive_and_trims() {
    assert_eq!(LogLevel::parse(" OFF "), Some(LogLevel::Off));
    assert_eq!(LogLevel::parse("Info"), Some(LogLevel::Info));
    assert_eq!(LogLevel::parse("\tDEBUG\n"), Some(LogLevel::Debug));
    assert_eq!(LogLevel::parse("Trace"), Some(LogLevel::Trace));
}

#[test]
fn log_level_parse_accepts_convenience_aliases() {
    // Numeric + truthy synonyms map to Info (matches the existing
    // `VOICEPI_DEBUG=1` convention — users habitually typing `1`
    // shouldn't get bumped to Debug/Trace and flood the tee file).
    // `err` shortens `error`; `verbose`/`all`/`full` map to Trace
    // to mirror the rest of the Windows diagnostics doc.
    assert_eq!(LogLevel::parse("0"), Some(LogLevel::Off));
    assert_eq!(LogLevel::parse("false"), Some(LogLevel::Off));
    assert_eq!(LogLevel::parse("no"), Some(LogLevel::Off));
    assert_eq!(LogLevel::parse("1"), Some(LogLevel::Info));
    assert_eq!(LogLevel::parse("true"), Some(LogLevel::Info));
    assert_eq!(LogLevel::parse("yes"), Some(LogLevel::Info));
    assert_eq!(LogLevel::parse("on"), Some(LogLevel::Info));
    assert_eq!(LogLevel::parse("err"), Some(LogLevel::Error));
    assert_eq!(LogLevel::parse("warning"), Some(LogLevel::Warn));
    assert_eq!(LogLevel::parse("dbg"), Some(LogLevel::Debug));
    assert_eq!(LogLevel::parse("verbose"), Some(LogLevel::Trace));
    assert_eq!(LogLevel::parse("all"), Some(LogLevel::Trace));
    assert_eq!(LogLevel::parse("full"), Some(LogLevel::Trace));
}

#[test]
fn log_level_parse_empty_string_is_info() {
    // Empty is treated as unset → the release default (Info). The
    // init_from_env path also picks Info for a missing env var so the
    // two branches agree.
    assert_eq!(LogLevel::parse(""), Some(LogLevel::Info));
    assert_eq!(LogLevel::parse("   "), Some(LogLevel::Info));
}

#[test]
fn log_level_parse_unknown_returns_none() {
    // Typos like `debgu` or `everything` must NOT silently map to a
    // valid level — the caller (init_from_env) turns None into a
    // warning plus an Info default so a support log shows the
    // mistake.
    assert!(LogLevel::parse("debgu").is_none());
    assert!(LogLevel::parse("everything").is_none());
    assert!(LogLevel::parse("critical").is_none());
}

#[test]
fn log_level_as_str_is_stable_short_name() {
    // Pinned so grep strings in support runbooks
    // (`grep VOICEPI_LOG=debug`) keep working.
    assert_eq!(LogLevel::Off.as_str(), "off");
    assert_eq!(LogLevel::Error.as_str(), "error");
    assert_eq!(LogLevel::Warn.as_str(), "warn");
    assert_eq!(LogLevel::Info.as_str(), "info");
    assert_eq!(LogLevel::Debug.as_str(), "debug");
    assert_eq!(LogLevel::Trace.as_str(), "trace");
}

#[test]
fn init_from_env_reads_env_var_and_caches_into_atomic() {
    // Hold DIAG_WRITER_LOCK too so we don't flip `LEVEL` to `Off`
    // mid-log for a concurrent writer-installing test — the `#651`
    // sink gate makes level and writer state cross-dependent
    // (Codex P2 #665 discussion PRRT_kwDOSfNjQs6UYXrm). Acquire
    // the diag lock BEFORE the env lock to match the lock order in
    // every other diag-touching test in this file.
    let _diag_guard = diag_test_lock();
    let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let prev = std::env::var(LOG_ENV_VAR).ok();

    std::env::remove_var(LOG_ENV_VAR);
    reset_level_for_tests();
    assert_eq!(
        init_from_env(),
        LogLevel::Info,
        "unset env var must default to Info so nothing changes for \
         existing users (matches the release default)"
    );
    assert_eq!(current_level(), LogLevel::Info);
    assert!(info_enabled());
    assert!(!debug_enabled());
    assert!(!trace_enabled());

    std::env::set_var(LOG_ENV_VAR, "debug");
    reset_level_for_tests();
    assert_eq!(init_from_env(), LogLevel::Debug);
    assert_eq!(current_level(), LogLevel::Debug);
    assert!(info_enabled(), "debug implies info");
    assert!(debug_enabled());
    assert!(!trace_enabled());

    std::env::set_var(LOG_ENV_VAR, "trace");
    reset_level_for_tests();
    assert_eq!(init_from_env(), LogLevel::Trace);
    assert_eq!(current_level(), LogLevel::Trace);
    assert!(info_enabled(), "trace implies info");
    assert!(debug_enabled(), "trace implies debug");
    assert!(trace_enabled());

    std::env::set_var(LOG_ENV_VAR, "off");
    reset_level_for_tests();
    assert_eq!(init_from_env(), LogLevel::Off);
    assert!(!info_enabled());
    assert!(!debug_enabled());
    assert!(!trace_enabled());

    // Unknown value → warn + Info default. We can't easily observe
    // the warning line from a unit test (it goes through the tee),
    // but we CAN pin the level fallback.
    std::env::set_var(LOG_ENV_VAR, "debgu");
    reset_level_for_tests();
    assert_eq!(
        init_from_env(),
        LogLevel::Info,
        "an unknown VOICEPI_LOG value must fall back to Info \
         (never silently promote to Trace or demote to Off)"
    );

    match prev {
        Some(v) => std::env::set_var(LOG_ENV_VAR, v),
        None => std::env::remove_var(LOG_ENV_VAR),
    }
}

/// Codex P2 #651 r3663372988: `VOICEPI_LOG=off` must actually
/// silence the diagnostic sink — the docs promise "Nothing, not
/// even startup markers". Before the sink-level gate the GUI
/// startup marker and every unconditional lifecycle line still
/// wrote through, so a user who set `off` still saw file growth.
///
/// This asserts the sink itself early-returns at `Off`, without
/// depending on individual call sites remembering to gate. Runs
/// on every platform because `install_gui_diagnostic_log` +
/// `log!` cover the tee path on non-Windows too.
#[test]
fn write_line_is_silenced_when_level_is_off() {
    use std::io::Read;
    let _lock = diag_test_lock();
    let _env_guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let prev = std::env::var(LOG_ENV_VAR).ok();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("silenced.log");
    install_gui_diagnostic_log(&path).expect("install diag");

    // Level = Off: the sink must drop the message before either
    // stderr or the tee file is touched. The tee-file path is the
    // observable half; if it stays empty we know the sink obeyed.
    std::env::set_var(LOG_ENV_VAR, "off");
    reset_level_for_tests();
    assert_eq!(init_from_env(), LogLevel::Off);

    crate::diag::log!("[test] this line must not appear at Off");
    crate::diag::log!("[test] neither must this one");

    let mut contents = String::new();
    std::fs::File::open(&path)
        .expect("open diag file")
        .read_to_string(&mut contents)
        .expect("read diag file");
    assert!(
        contents.is_empty(),
        "VOICEPI_LOG=off must silence write_line; tee file grew to {contents:?}"
    );

    // Sanity: bumping the level back to Info restarts the sink
    // (no residual latch), so this isn't a one-way trap.
    std::env::set_var(LOG_ENV_VAR, "info");
    reset_level_for_tests();
    assert_eq!(init_from_env(), LogLevel::Info);
    crate::diag::log!("[test] visible-after-off");
    let mut contents = String::new();
    std::fs::File::open(&path)
        .expect("re-open diag file")
        .read_to_string(&mut contents)
        .expect("re-read diag file");
    assert!(
        contents.contains("[test] visible-after-off"),
        "sink must resume after level moves off Off, got {contents:?}"
    );

    match prev {
        Some(v) => std::env::set_var(LOG_ENV_VAR, v),
        None => std::env::remove_var(LOG_ENV_VAR),
    }
}

/// Installing twice must not fail, and the second install SWAPS
/// the writer to the new path. Tests rely on this so each test's
/// temp file receives its own writes rather than accumulating in
/// a sibling test's leftover file. Production callers install
/// exactly once (from `whisper-dictate-gui::main`) so the swap
/// semantics are invisible there.
#[test]
fn install_gui_diagnostic_log_swaps_writer_on_reinstall() {
    use std::io::Read;
    let _lock = diag_test_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let path_a = dir.path().join("first.log");
    install_gui_diagnostic_log(&path_a).expect("first install");
    install_gui_diagnostic_log(&path_a).expect("second install same path");
    let path_b = dir.path().join("second.log");
    install_gui_diagnostic_log(&path_b).expect("third install different path");

    // After the swap, log! writes must land in path_b (the newest
    // install), not path_a. This is what distinguishes the
    // "swap" contract from the earlier "first-writer-wins" one.
    crate::diag::log!("[test] after-swap");
    let mut contents = String::new();
    std::fs::File::open(&path_b)
        .expect("open swapped file")
        .read_to_string(&mut contents)
        .expect("read swapped file");
    assert!(
        contents.contains("[test] after-swap"),
        "re-install must swap the writer so new log! calls go to the newest path: {contents:?}",
    );
}

// -----------------------------------------------------------------------
// Codex P1 #644 r3658983548 — fallible stderr write.
//
// The stderr side of the tee used to be `eprintln!`, which panics on
// `write_all` failure. On Windows the hidden-subsystem launcher / a
// consumer closing a redirected pipe can leave stderr in exactly that
// "closed / invalid" state — the unconditional session marker at
// startup would then abort startup, or a later diagnostic would kill
// the calling thread, losing the very file record intended to diagnose
// the failure. The fix routes the stderr write through
// `io::stderr().lock()` + `writeln!` + `let _ =` so the Err is
// swallowed and the file-append side below still runs.
//
// The exact bug is a Windows-only panic that Linux CI cannot
// reproduce (there is no test-friendly way to close fd 2 from inside
// the same process without polluting other tests' output). The
// regression test therefore pins the STRUCTURE of the fix: the
// production `write_line` function's source must not use `eprintln!`
// for the stderr tee. Un-fixed code contained `eprintln!("{line}")`
// and this assertion would fail.
// -----------------------------------------------------------------------

/// One function body lifted out of a production source file, for the
/// structural scanners below.
///
/// `code` has `//` line comments stripped — assert *absence* of a
/// pattern against this field, so a mention inside the fix's own
/// rationale comment cannot false-fail the check. `raw` keeps the
/// comments — assert *presence* against this one, and use it in
/// failure messages so the operator sees the real source.
///
/// `pub(crate)` because the same mechanism guards the Windows raw-hook
/// callback in `win_raw_hook_tests.rs` — that callback is an
/// `extern "system"` fn only the OS can invoke, so a structural scan is
/// the only way to pin "never write synchronously from inside an
/// LL-hook callback".
pub(crate) struct FnBody {
    pub(crate) raw: String,
    pub(crate) code: String,
}

/// Read `rel_path` and return the body of the function introduced by
/// `fn_marker` (the literal signature text up to and including its
/// opening `{`).
///
/// Extracted so the three structural scanners below share one
/// implementation instead of repeating the read + brace-walk +
/// comment-strip mechanism verbatim. Only the *mechanism* is shared:
/// which file, which function, which pattern and which failure message
/// — everything that documents what each test guards — stays at the
/// call site.
///
/// The walk is a brace-depth counter rather than a real parse because
/// `syn` is not a project dependency: start at the opening `{`, advance
/// until the matching `}` at depth 0. None of the scanned functions
/// contain unbalanced braces inside string/char literals today; a
/// future refactor that introduced one would need to update this
/// helper, which is acceptable — the point of these tests is structural
/// discipline, not a general-purpose Rust parser.
pub(crate) fn scan_fn_body(rel_path: &str, fn_marker: &str) -> FnBody {
    // `cargo test` runs from the crate root (src/rust), but some
    // invocations run from the repo root — try both.
    let src = std::fs::read_to_string(rel_path)
        .or_else(|_| std::fs::read_to_string(rel_path.trim_start_matches("src/rust/")))
        .unwrap_or_else(|err| {
            panic!("{rel_path} must be readable from the test working dir ({err})")
        });
    let fn_start = src
        .find(fn_marker)
        .unwrap_or_else(|| panic!("{fn_marker:?} must exist in {rel_path}"));
    // The marker may or may not include the opening brace; locate it
    // either way so callers can pass a truncated signature.
    let open_brace_offset = src[fn_start..]
        .find('{')
        .map(|i| fn_start + i)
        .unwrap_or_else(|| panic!("{fn_marker:?} in {rel_path} must have an opening brace"));
    let mut depth: i32 = 0;
    let mut end: Option<usize> = None;
    for (i, ch) in src[open_brace_offset..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open_brace_offset + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let fn_end =
        end.unwrap_or_else(|| panic!("{fn_marker:?} in {rel_path} must have a matching `}}`"));
    let raw = src[fn_start..fn_end].to_owned();
    let code = raw
        .lines()
        .map(|line| line.find("//").map_or(line, |idx| &line[..idx]))
        .collect::<Vec<_>>()
        .join("\n");
    FnBody { raw, code }
}

/// A writer whose every `write` / `flush` fails with `BrokenPipe` —
/// the exact `io::Error` a closed redirected-stderr consumer produces
/// on both Windows and Unix. Counts attempts so the test can prove the
/// sink really tried to write rather than skipping the branch.
struct FailingWriter {
    write_attempts: std::cell::Cell<usize>,
}

impl FailingWriter {
    fn new() -> Self {
        Self {
            write_attempts: std::cell::Cell::new(0),
        }
    }
}

impl std::io::Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        self.write_attempts.set(self.write_attempts.get() + 1);
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "simulated closed stderr consumer",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "simulated closed stderr consumer",
        ))
    }
}

/// Codex P2 #668 discussion 3666529224 — drive the failing-stderr path
/// for real instead of only banning `eprintln!` textually.
///
/// The scanner below cannot catch `writeln!(handle, ...).unwrap()` or
/// `.expect()`, which would restore the exact panic the #644 fix
/// removed. This test passes a writer that always returns `BrokenPipe`
/// and asserts BOTH halves of the contract:
///
///  1. `write_line_to` does not panic (an `unwrap`/`expect` regression
///     unwinds here and fails the test).
///  2. The diagnostic-file append still happens — a dead stderr must
///     not cost us the tee record, which is the entire reason the GUI
///     diagnostic path exists.
#[test]
fn write_line_to_survives_a_failing_stderr_sink() {
    use std::io::Read;

    let _guard = diag_test_lock();
    // The `Off` level short-circuits `write_line` before the sink, so
    // make sure we're at a level that actually writes.
    let _env_guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let prev_env = std::env::var(LOG_ENV_VAR).ok();
    std::env::set_var(LOG_ENV_VAR, "info");
    reset_level_for_tests();
    init_from_env();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("failing-stderr.log");
    install_gui_diagnostic_log(&path).expect("install tee sink");

    let mut failing = FailingWriter::new();
    // If `write_line_to` ever regresses to `unwrap()` / `expect()` on
    // the stderr side, this call panics and the test fails — which is
    // precisely the regression the textual scanner cannot see.
    crate::diag::write_line_to(
        &mut failing,
        "t=0ms [test] stderr is dead but the tee must live",
    );

    assert!(
        failing.write_attempts.get() > 0,
        "the sink must actually attempt the stderr write — if it \
         short-circuited, this test would prove nothing about the \
         failing-writer path"
    );

    let mut contents = String::new();
    std::fs::File::open(&path)
        .expect("open tee file")
        .read_to_string(&mut contents)
        .expect("read tee file");
    assert!(
        contents.contains("stderr is dead but the tee must live"),
        "the diagnostic-file append MUST still happen when the stderr \
         write fails — losing the tee record on a closed/redirected \
         stderr is exactly the Windows failure the fallible-write \
         contract exists to prevent. Codex P1 #644 r3658983548 + \
         Codex P2 #668 3666529224. Tee contents: {contents:?}"
    );

    match prev_env {
        Some(v) => std::env::set_var(LOG_ENV_VAR, v),
        None => std::env::remove_var(LOG_ENV_VAR),
    }
    reset_level_for_tests();
}

#[test]
fn write_line_does_not_use_eprintln_or_panicking_writes_for_stderr_tee() {
    // Belt to the runtime test's braces: ban the panicking spellings
    // textually as well, so a regression is caught at review time even
    // before the failing-sink test runs. `write_line_to` is the sink
    // half that owns both writes (see `diag.rs`).
    let body = scan_fn_body(
        "src/rust/diag.rs",
        "pub(crate) fn write_line_to<W: Write>(mut stderr_sink: W, line: &str) {",
    );
    assert!(
        !body.code.contains("eprintln!"),
        "write_line_to MUST NOT use `eprintln!` — it panics on stderr \
         write failure and closes the GUI diagnostic path on Windows. \
         Codex P1 #644 r3658983548. Offending function body:\n{}",
        body.raw
    );
    // `unwrap()` / `expect()` on either write would restore the same
    // panic that `eprintln!` caused — the failing-sink test above
    // catches it at runtime, this catches it by inspection.
    for banned in ["unwrap()", "expect("] {
        assert!(
            !body.code.contains(banned),
            "write_line_to MUST NOT use `{banned}` on its writes — a \
             closed / redirected stderr would panic and abort the GUI \
             startup marker, losing the tee record. Every Err must be \
             discarded via `let _ =`. Codex P2 #668 discussion \
             3666529224. Offending function body:\n{}",
            body.raw
        );
    }
    // Sanity: the sink must still write to BOTH destinations. A
    // regression that dropped either side would still pass the bans above.
    assert!(
        body.raw.contains("stderr_sink"),
        "write_line_to must still tee to the stderr sink. Offending body:\n{}",
        body.raw
    );
    assert!(
        body.raw.contains("diag_file()"),
        "write_line_to must still append to the diagnostic file. \
         Offending body:\n{}",
        body.raw
    );
    // Codex P1 #681 PRRT_kwDOSfNjQs6UfWDv: the stderr guard must be
    // released BEFORE the blocking tee lock, or a wedged AppData volume
    // pins the process stderr lock and `write_line_nonblocking` can
    // never reach its `try_lock`. The runtime companion is
    // `a_wedged_tee_write_does_not_pin_the_stderr_lock_against_the_teardown_warning`.
    let drop_at = body.code.find("drop(stderr_sink)");
    let tee_at = body.code.find("diag_file()");
    assert!(
        matches!((drop_at, tee_at), (Some(d), Some(t)) if d < t),
        "write_line_to must `drop(stderr_sink)` BEFORE it locks \
         `diag_file()`; holding the process stderr guard across the tee \
         write lets a wedged sink pin process exit past \
         DIAG_DRAIN_DEADLINE. Offending body:\n{}",
        body.raw
    );
}

// -----------------------------------------------------------------------
// Codex P2 #668 discussion 3665200207 — the `handle_self_test_hotkey_boot`
// warning path in `main.rs` (added by the #644 sweep to preserve
// config-load errors) originally used `eprintln!` for the "config load
// failed; continuing with --chord override" line. That is the SAME
// panic-on-closed-stderr class of failure this commit removed from
// `diag::write_line` — a self-test invoked from a hidden Windows
// launcher (or with a closed / redirected stderr consumer) would abort
// before `run_boot_test` ever runs. The fix routes the warning through
// `diag::write_line` (fallible) so a dead stderr is swallowed and the
// self-test's stdout report still lands. This scanner catches any
// regression that reintroduces `eprintln!` inside that function body.
// -----------------------------------------------------------------------

#[test]
fn hotkey_boot_self_test_dispatcher_does_not_use_eprintln_for_config_warning() {
    let body = scan_fn_body("src/rust/main.rs", "fn handle_self_test_hotkey_boot(");
    assert!(
        !body.code.contains("eprintln!"),
        "handle_self_test_hotkey_boot MUST NOT use `eprintln!` — it panics \
         on stderr write failure and would abort the CLI before \
         `run_boot_test` runs on a closed / redirected stderr (the exact \
         Windows/hidden-launcher failure `diag::write_line` was rewritten \
         to survive). Route the warning through \
         `whisper_dictate_app::diag::write_line(...)` instead. Codex P2 \
         #668 discussion 3665200207. Offending function body:\n{}",
        body.raw
    );
    // Sanity: the fix's warning must still land SOMEWHERE — if a future
    // refactor deleted the warning entirely, the "config load failed"
    // signal an operator debugging a wedge relies on would be lost.
    assert!(
        body.raw.contains("config load failed"),
        "the config-load-failed warning path must still emit its \
         diagnostic; a regression that dropped the warning would make a \
         corrupt-config self-test silently look like a normal `--chord` \
         run. Codex P2 #644 r3658983556 + Codex P2 #668 3665200207."
    );
}

// -----------------------------------------------------------------------
// Codex P1 #668 discussion 3665741341 — the tracker's `[chord]` trace
// runs on the LL-hook callback thread (via `dispatch_raw_event` from
// rdev's cb) and therefore MUST use `log_async!`, not `log!`. Before
// the fix, `tracker.rs` called `crate::diag::log!` synchronously,
// which on Windows can exceed the `WH_KEYBOARD_LL` time budget on a
// stalled AppData sink and silently unhook the callback — defeating
// the earlier off-callback queue for the rdev boundary trace.
//
// Structural scanner: pin the fix by rejecting any synchronous
// `crate::diag::log!` inside `KeyTracker::handle`. The debug branch
// is the only diagnostic call site there today; a future addition
// that used `log!` instead of `log_async!` would re-introduce the
// wedge.
// -----------------------------------------------------------------------

#[test]
fn tracker_handle_does_not_use_synchronous_diag_log_on_callback_path() {
    let body = scan_fn_body(
        "src/rust/hotkey/manager/tracker.rs",
        "pub fn handle(&mut self, event: &RawKeyEvent) -> Option<TrackerOutput> {",
    );
    assert!(
        !body.code.contains("crate::diag::log!"),
        "KeyTracker::handle MUST NOT use `crate::diag::log!` — it \
         runs on the rdev LL-hook callback thread on Windows and a \
         synchronous write on a stalled diag sink would silently \
         unhook the callback. Use `crate::diag::log_async!` instead. \
         Codex P1 #668 discussion 3665741341. Offending function body:\n{}",
        body.raw
    );
    // Sanity: the fix's async path must still be present — a
    // regression that dropped the trace entirely would remove the
    // `[chord]` line that the redaction tests below rely on.
    assert!(
        body.raw.contains("log_async!"),
        "KeyTracker::handle must still emit the `[chord]` trace at \
         debug level (via `crate::diag::log_async!`). Regression \
         that dropped the trace would silence the wedge diagnostic. \
         Codex P1 #668 3665741341."
    );
}

#[test]
fn write_line_stays_stable_across_many_calls() {
    // Belt-and-braces: call `write_line` several thousand times to
    // exercise both the stderr and file paths and ensure the fallible
    // stderr write never panics on ordinary stdio. A regression that
    // brought back `eprintln!` still passes this test (because CI's
    // stderr is fine), but the structural test above catches that;
    // the pair together bounds the fix.
    let _guard = diag_test_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("stability.log");
    install_gui_diagnostic_log(&path).expect("install stability sink");
    for i in 0..2000 {
        crate::diag::log!("[test] stability line #{i}");
    }
    // If we got here, the loop completed without panicking — the
    // fallible-writer contract held for the whole burst.
}

// -----------------------------------------------------------------------
// Drop accounting for the off-callback async queue.
//
// `enqueue_async` sheds on a full queue (it must — blocking would park
// the Windows LL-hook callback and re-create the wedge the queue exists
// to prevent). Before this landed the shed was SILENT: a reader of
// `gui-diagnostic.log` saw the same thing whether the callback was
// quiet or whether the queue had thrown away a burst, which makes the
// trace untrustworthy on exactly the slow-AppData scenario it is meant
// to diagnose. The fix counts drops and has the writer thread announce
// them as ONE coalesced `[diag-async] dropped=<n>` line before the next
// record it writes.
// -----------------------------------------------------------------------

/// What [`flood_a_stalled_async_queue`] observed, so the regression
/// test below is nothing but behavioural assertions.
struct StalledQueueRun {
    /// How long the flood itself took. The enqueue half must never
    /// block, so this is the "did it park the LL-hook callback" probe.
    flood_elapsed: std::time::Duration,
    /// Records the queue shed, read while NOTHING is consuming.
    shed: u64,
    /// Records the queue accepted, likewise read with no consumer.
    accepted: usize,
    /// Every line the writer handed to its sink, in order.
    recorded: Vec<String>,
    /// `pending` after the writer drained — the `flush_async_for_tests`
    /// contract.
    pending_after: usize,
    /// `dropped` after the writer drained — the counter must reset so
    /// the next burst reports its own size, not a running total.
    dropped_after: u64,
    /// Drops the ledger still considers UNNAMED after the writer drained.
    /// Non-zero would mean a gap the log never told anyone about.
    unnamed_after: u64,
}

/// Drive `enqueue_async_into` / `run_async_writer_loop` — the exact
/// halves `enqueue_async` / `ensure_async_writer` wire to the
/// process-wide statics — against a tiny channel, so the "queue filled
/// while the sink could not keep up" path is reachable at all: the
/// production queue is 256 deep and its sink is a file write no test
/// can pause.
///
/// ## Why this is two sequential phases and not one concurrent one
///
/// The original shape ran the writer on a scoped thread whose sink
/// parked on a condvar, flooded the channel "while the sink was
/// stalled", and then read `dropped`. That was racy and failed roughly
/// one run in six (CI run 30379264960; reproduced locally at 2/20 in
/// isolation, far worse under a loaded full-suite run): the writer
/// thread is free to `recv` its first record at any point DURING the
/// flood, and draining a record takes the outstanding count with it, so
/// the post-flood `dropped.load()` only counted the drops that happened
/// afterwards — observed 123 and 133 where the test demanded >= 199.
///
/// Production is *correct* there; the taken drops are not lost, they
/// are carried into the marker the writer emits. It was the test's
/// observation point that was racy — reading a counter a live writer is
/// entitled to take. So the two halves of the contract are observed at
/// points where nothing else can be running:
///
/// 1. **Accounting** — flood with NO consumer at all. `sync_channel`
///    buffers exactly `capacity` and `try_send` reports `Full` for
///    every one after that, so the split is exactly `capacity`
///    accepted / `overflow` shed, every run. This is also the strictest
///    possible version of "the enqueue must not block": with no
///    receiver ever running, a blocking `send` deadlocks instead of
///    merely being slow.
/// 2. **Reporting** — only then run the writer loop, on this thread,
///    over the already-closed channel. It drains, exits, and the sink
///    `Vec` is complete with no join to wait on. Running it inline also
///    removes the old hang hazard entirely: there is no parked thread
///    for a failing assertion to unwind into.
fn flood_a_stalled_async_queue(capacity: usize, overflow: usize) -> StalledQueueRun {
    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(capacity);

    // ---- Phase 1: the queue ACCOUNTS for what it sheds. ----
    let flood_started = std::time::Instant::now();
    for i in 0..(capacity + overflow) {
        crate::diag::enqueue_async_into(&tx, &admission, &pending, &dropped, format!("flood #{i}"));
    }
    let flood_elapsed = flood_started.elapsed();
    let shed = dropped.unbound();
    let accepted = pending.load(Ordering::Relaxed);

    // ---- Phase 2: the writer REPORTS them as one coalesced marker. ----
    //
    // Close the channel first so the loop drains what survived and
    // returns; run it inline so there is no thread to join and no
    // ordering left to chance.
    drop(tx);
    let mut recorded: Vec<String> = Vec::new();
    crate::diag::run_async_writer_loop(rx, capacity, &pending, &dropped, |line| {
        recorded.push(line.to_owned());
    });

    StalledQueueRun {
        flood_elapsed,
        shed,
        accepted,
        recorded,
        pending_after: pending.load(Ordering::Relaxed),
        dropped_after: dropped.unbound(),
        unnamed_after: dropped.unnamed(),
    }
}

/// The regression test for the shed-visibility fix, plus the marker's
/// POSITION within the drained trace.
///
/// Un-fixed behaviour, two distinct regressions:
///
/// * `if tx.try_send(message).is_ok()` (no accounting at all): the
///   flood is accepted-or-discarded with no bookkeeping, `shed` stays 0
///   and the sink sees records only — no marker line at all.
/// * Writer reads a shared counter at write time (the pre-#680-review
///   shape): the marker is emitted before the FIRST record it dequeues.
///   Those `CAPACITY` records were accepted BEFORE the gap, so the log
///   claims the trace broke up to a whole backlog earlier than it did —
///   `recorded.first()` is the marker instead of `flood #0`.
#[test]
fn bounded_async_queue_sheds_and_reports_a_coalesced_dropped_marker() {
    // Belt-and-braces: the pipeline halves under test are parameterised
    // away from the global writer slot and the LEVEL atomic, but a
    // future rework that reached for either would otherwise flake
    // against the rest of the suite.
    let _guard = diag_test_lock();

    const CAPACITY: usize = 8;
    const OVERFLOW: usize = 200;
    let run = flood_a_stalled_async_queue(CAPACITY, OVERFLOW);
    let recorded = &run.recorded;

    assert!(
        run.flood_elapsed < std::time::Duration::from_secs(2),
        "enqueue must never block on a full queue (the whole \
         WH_KEYBOARD_LL callback budget is a few ms); flooding {} \
         records took {:?}",
        CAPACITY + OVERFLOW,
        run.flood_elapsed
    );
    assert_eq!(
        run.accepted, CAPACITY,
        "a capacity-{CAPACITY} channel with no consumer must accept \
         exactly {CAPACITY} records and shed the rest; accepted={}",
        run.accepted
    );
    assert_eq!(
        run.shed,
        OVERFLOW as u64,
        "a full queue must ACCOUNT for what it shed; after flooding {} \
         records into a capacity-{CAPACITY} channel exactly {OVERFLOW} \
         must be counted as dropped, got {}. dropped=0 means the \
         drop is silent again and `gui-diagnostic.log` cannot \
         distinguish a quiet callback from a shed burst",
        CAPACITY + OVERFLOW,
        run.shed
    );

    // The memory bound, restated on the output side: the writer can
    // only ever have held `capacity` records, plus the one marker.
    assert_eq!(
        recorded.len(),
        CAPACITY + 1,
        "the writer must emit exactly the {CAPACITY} surviving records \
         plus ONE marker; saw {} lines: {recorded:?}",
        recorded.len()
    );

    let markers: Vec<&String> = recorded
        .iter()
        .filter(|line| line.contains("[diag-async] dropped="))
        .collect();
    assert_eq!(
        markers.len(),
        1,
        "drops must be reported as ONE coalesced marker, never one line \
         per drop (that would be unbounded write amplification against \
         the same stalled sink that caused the drops) and never zero \
         (that is the silent shed this test exists to prevent); got \
         {markers:?} out of {recorded:?}"
    );
    assert_eq!(
        markers[0],
        &crate::diag::async_dropped_marker(run.shed, CAPACITY),
        "the marker must name the exact shed count and the capacity that \
         was exceeded, so a log reader can size the gap"
    );
    // POSITION is the point: `Full` sheds the NEWEST record, so every
    // record still in the queue was accepted BEFORE the gap and must be
    // written before the marker. A marker at the front would date the
    // gap a whole backlog too early for a reader correlating `t=<ms>`
    // prefixes against the moment PTT died.
    let (survivors, tail) = recorded.split_at(CAPACITY);
    let expected: Vec<String> = (0..CAPACITY).map(|i| format!("flood #{i}")).collect();
    assert_eq!(
        survivors.iter().map(String::as_str).collect::<Vec<_>>(),
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "every ACCEPTED record must reach the sink, in order, BEFORE the \
         marker for a gap that happened after they were accepted; \
         recorded={recorded:?}"
    );
    assert_eq!(
        tail,
        [markers[0].clone()],
        "the marker must sit at the queue position of the gap — after \
         the records the queue had already accepted, not ahead of them; \
         recorded={recorded:?}"
    );
    assert_eq!(
        run.pending_after, 0,
        "every record the queue ACCEPTED must have reached the sink; a \
         non-zero pending count would stall `flush_async_for_tests`"
    );
    assert_eq!(
        run.dropped_after, 0,
        "emitting the marker must RESET the counter, so the next burst \
         reports its own size and not a running total"
    );
    assert_eq!(
        run.unnamed_after, 0,
        "every shed record must end up NAMED by a marker; a residue here \
         is a gap the log never told the reader about"
    );
}

/// A gap at the very END of a trace must still reach the log.
///
/// This is the wedge case the whole accounting exists for: PTT dies,
/// the queue sheds the tail of the burst, and then NOTHING else ever
/// happens. The writer's sender is process-wide and never dropped, so a
/// writer parked in a plain blocking `recv()` never wakes again and the
/// final shed burst stays silent forever — exactly the run an operator
/// would be reading the log to explain (Codex P2 #680 comment
/// 3667524121).
///
/// The counter is bumped directly rather than through
/// `enqueue_async_into` on purpose: a shed REQUIRES a full queue, and a
/// live writer drains the queue, so "a drop lands while the writer is
/// parked on an empty queue" cannot be scheduled deterministically from
/// the producer side. The bump is byte-for-byte what
/// `enqueue_async_into` does on `TrySendError::Full`, and the contract
/// under test is the writer's: an outstanding count must surface with
/// no record to carry it.
///
/// Un-fixed behaviour (`while let Ok(line) = rx.recv()`): the writer
/// parks forever, the polling loop below times out, and `observed` is
/// empty — the trailing `close_burst_with_pending_drops` only runs once
/// `tx` is dropped, which is why `observed` is snapshotted BEFORE that.
#[test]
fn a_shed_burst_that_ends_the_trace_still_reaches_the_log() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const SHED: u64 = 7;

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);
    let recorded = std::sync::Mutex::new(Vec::<String>::new());
    let expected = crate::diag::async_dropped_marker(SHED, CAPACITY);

    // Everything that could panic happens AFTER the scope: a failing
    // assertion inside would unwind while the writer is parked and the
    // implicit join would hang forever.
    let observed = std::thread::scope(|scope| {
        scope.spawn(|| {
            crate::diag::run_async_writer_loop(rx, CAPACITY, &pending, &dropped, |line| {
                recorded
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(line.to_owned());
            });
        });

        // The producer's `Full` branch, with no record before it and
        // none after it.
        dropped.shed_for_tests(SHED);

        // Generous by ~20x against the writer's park interval so a
        // loaded CI box cannot turn a pass into a flake; a healthy run
        // leaves this loop in well under a second.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen: Vec<String> = Vec::new();
        while std::time::Instant::now() < deadline {
            seen = recorded
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            if !seen.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Snapshot taken; releasing the sender lets the loop return so
        // the scope can join it.
        drop(tx);
        seen
    });

    assert_eq!(
        observed,
        vec![expected],
        "a burst shed after the LAST record must still be reported: the \
         writer's sender is never dropped, so a writer that only wakes \
         on the next record never reports the gap that ended the trace. \
         Saw {observed:?}"
    );
}

// -----------------------------------------------------------------------
// Marker coalescing across a whole overload BURST (Codex P2 #680 comment
// 3668174780).
//
// The shed count rides on the first record accepted after the gap, which
// is right for POSITION but, on its own, wrong for VOLUME: under a
// sustained overload (the documented `VOICEPI_LOG=debug` mouse stream
// against a stalled AppData volume) every dequeue frees exactly one slot,
// the producer refills it at once, and more records are shed while the
// writer is still writing — so every record carries a non-zero count and
// the writer emits nearly ONE MARKER PER SURVIVING RECORD. That doubles
// the write volume against the sink that was already too slow.
//
// The two tests below bound the two halves: a lock-step concurrent
// producer/writer for the amplification itself (the prefilled,
// closed-channel harness above structurally cannot expose it — it has no
// producer running while the writer writes), and a fully synchronous one
// for the episode state machine's exact output.
// -----------------------------------------------------------------------

/// Lock-step handshake budget. Orders of magnitude above the microseconds
/// a handshake actually costs: it exists ONLY so a harness bug surfaces
/// as an assertable flag instead of hanging CI forever.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// What [`drive_a_saturated_async_queue`] observed. Every field is read
/// after all threads have joined, so no assertion can unwind into a
/// parked writer.
struct SaturatedRun {
    /// Every line the writer handed to its sink, in order.
    recorded: Vec<String>,
    /// `pending` after the pipeline drained.
    pending_after: usize,
    /// Set when a lock-step handshake timed out. A harness failure, not a
    /// behavioural one — asserted separately so the two never get
    /// confused.
    handshake_failed: bool,
}

/// Hold a real producer thread and the real writer loop in a SATURATED
/// steady state, deterministically.
///
/// ## Why this needs to be concurrent, and how it stays deterministic
///
/// The amplification only exists when records are shed WHILE the writer
/// is writing, so a harness that floods first and drains afterwards
/// cannot see it: with no producer running, every dequeued record but
/// the first carries a zero count. A free-running producer thread would
/// show it, but a previous free-running test on this branch failed ~1 run
/// in 6 (CI run 30379264960) — a probabilistic test that blocks a stack
/// is worse than no test.
///
/// So the two threads are real but LOCK-STEP. The writer's sink is the
/// seam: it runs on the writer thread, immediately after a record was
/// dequeued and therefore exactly while "the previous marker and record
/// are being written". It hands the producer a token, the producer
/// enqueues one record (which fits the slot the dequeue just freed) plus
/// `shed_per_round` more (which cannot fit, and are shed), then hands the
/// token back. Nothing races: the queue depth, the accept/shed split and
/// the carried count are the same on every run.
///
/// The queue starts full — `sync_channel` buffers exactly `capacity` and
/// nothing consumes during the prefill — so the steady state is reached
/// on the very first dequeue rather than being waited for.
fn drive_a_saturated_async_queue(
    capacity: usize,
    rounds: usize,
    shed_per_round: usize,
) -> SaturatedRun {
    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let handshake_failed = std::sync::atomic::AtomicBool::new(false);
    let recorded = std::sync::Mutex::new(Vec::<String>::new());

    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(capacity);
    // Rendezvous pair: `go` is the writer saying "a slot just came free",
    // `done` is the producer saying "I have refilled it and shed the
    // rest". Unbounded channels so neither send can block.
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    for i in 0..capacity {
        crate::diag::enqueue_async_into(&tx, &admission, &pending, &dropped, format!("seed #{i}"));
    }

    let producer_tx = tx.clone();
    let admission_ref = &admission;
    let pending_ref = &pending;
    let dropped_ref = &dropped;
    let recorded_ref = &recorded;
    let failed_ref = &handshake_failed;

    std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            for round in 0..rounds {
                if go_rx.recv_timeout(HANDSHAKE_TIMEOUT).is_err() {
                    break;
                }
                // Exactly one fits the freed slot...
                crate::diag::enqueue_async_into(
                    &producer_tx,
                    admission_ref,
                    pending_ref,
                    dropped_ref,
                    format!("burst #{round}"),
                );
                // ...and everything after it arrives against a full queue.
                for i in 0..shed_per_round {
                    crate::diag::enqueue_async_into(
                        &producer_tx,
                        admission_ref,
                        pending_ref,
                        dropped_ref,
                        format!("shed #{round}.{i}"),
                    );
                }
                if done_tx.send(()).is_err() {
                    break;
                }
            }
            // `producer_tx` drops here, so once the writer has also seen
            // the outer `tx` go away it can disconnect and return.
        });

        scope.spawn(move || {
            let mut handshakes = 0usize;
            crate::diag::run_async_writer_loop(rx, capacity, pending_ref, dropped_ref, |line| {
                recorded_ref
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(line.to_owned());
                // Markers are not records: driving the producer from one
                // would change the accept/shed split depending on how
                // many markers the implementation chose to write, which
                // is precisely the variable under test.
                if line.starts_with("[diag-async]") || handshakes >= rounds {
                    return;
                }
                handshakes += 1;
                if go_tx.send(()).is_err() || done_rx.recv_timeout(HANDSHAKE_TIMEOUT).is_err() {
                    failed_ref.store(true, Ordering::Relaxed);
                }
            });
        });

        // The producer stops after exactly `rounds` handshakes; dropping
        // the last sender then lets the writer drain the tail, report it
        // and return, so the scope joins instead of hanging.
        let _ = producer.join();
        drop(tx);
    });

    let lines = recorded
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    SaturatedRun {
        recorded: lines,
        pending_after: pending.load(Ordering::Relaxed),
        handshake_failed: handshake_failed.load(Ordering::Relaxed),
    }
}

/// Pull the `dropped=<n>` count out of every marker line, in order.
///
/// Deliberately shape-agnostic (it matches both the episode-start and the
/// episode-summary marker) so the test states the CONTRACT — how many
/// markers, and does the last one name the whole burst — rather than
/// re-encoding one implementation's wording. The exact wording is pinned
/// by [`an_overload_burst_is_summarised_once_when_the_queue_catches_up`].
fn reported_drop_counts(recorded: &[String]) -> Vec<u64> {
    recorded
        .iter()
        .filter(|line| line.starts_with("[diag-async]"))
        .map(|line| {
            let tail = line
                .split("dropped=")
                .nth(1)
                .unwrap_or_else(|| panic!("marker without a `dropped=` count: {line}"));
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("marker with an unparseable count: {line}"))
        })
        .collect()
}

/// The regression for Codex P2 #680 comment 3668174780: a queue that
/// stays saturated must produce a marker count that scales with the
/// number of overload EPISODES, not with the number of surviving records.
///
/// Un-fixed behaviour (a marker for every record carrying a non-zero
/// count) with the constants below: 24 markers for 28 surviving records —
/// 0.86 markers per record, i.e. the trace is very nearly half marker
/// noise — and the last marker names 5, not the 120 records actually shed.
/// Fixed: 2 markers (one opening the episode, one summarising it) no
/// matter how long the overload lasts.
#[test]
fn a_saturated_queue_coalesces_its_markers_across_the_whole_burst() {
    // The halves under test are parameterised away from the global
    // writer slot and the LEVEL atomic, but hold the lock anyway so a
    // future rework that reached for either cannot flake the suite.
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const ROUNDS: usize = 24;
    const SHED_PER_ROUND: usize = 5;
    /// Every episode costs at most an opening marker and a closing
    /// summary. The point of the bound is that it does NOT contain
    /// `ROUNDS`.
    const MAX_MARKERS: usize = 2;

    let run = drive_a_saturated_async_queue(CAPACITY, ROUNDS, SHED_PER_ROUND);

    assert!(
        !run.handshake_failed,
        "the lock-step handshake timed out, so the run never reached the \
         saturated steady state this test measures; recorded={:?}",
        run.recorded
    );

    let markers = reported_drop_counts(&run.recorded);
    let records: Vec<&String> = run
        .recorded
        .iter()
        .filter(|line| !line.starts_with("[diag-async]"))
        .collect();

    let expected_records: Vec<String> = (0..CAPACITY)
        .map(|i| format!("seed #{i}"))
        .chain((0..ROUNDS).map(|i| format!("burst #{i}")))
        .collect();
    assert_eq!(
        records.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        expected_records
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        "the lock-step harness must produce exactly the prefilled seeds \
         plus one surviving record per round, in order — otherwise the \
         marker count below is being compared against a different run; \
         recorded={:?}",
        run.recorded
    );

    assert!(
        markers.len() <= MAX_MARKERS,
        "a sustained overload must be announced ONCE per episode, not once \
         per surviving record: got {} markers for {} surviving records \
         ({:.2} markers/record). Every dequeue frees exactly one slot, the \
         producer refills it and sheds more while the writer writes, so a \
         per-record marker doubles the write volume against the sink that \
         was already too slow. markers={markers:?}",
        markers.len(),
        records.len(),
        markers.len() as f64 / records.len() as f64,
    );
    assert!(
        !markers.is_empty(),
        "coalescing must not become silence — an overload that shed {} \
         records has to be visible in the log; recorded={:?}",
        ROUNDS * SHED_PER_ROUND,
        run.recorded
    );

    // Position, unchanged from the previous round: the gap opens on the
    // first record the producer enqueued against a full queue, which is
    // the record after the `CAPACITY` seeds and the first burst record.
    let first_marker_at = run
        .recorded
        .iter()
        .position(|line| line.starts_with("[diag-async]"))
        .expect("at least one marker");
    assert_eq!(
        first_marker_at,
        CAPACITY + 1,
        "the opening marker must still sit at the QUEUE POSITION of the \
         gap — after every record accepted before it, immediately ahead of \
         the first record accepted after it. recorded={:?}",
        run.recorded
    );

    assert_eq!(
        markers.last().copied(),
        Some((ROUNDS * SHED_PER_ROUND) as u64),
        "coalescing may delay a count, never lose one: the closing marker \
         must name every record the burst shed ({} of them). markers={markers:?}",
        ROUNDS * SHED_PER_ROUND
    );
    assert_eq!(
        run.pending_after, 0,
        "every record the queue ACCEPTED must still have reached the sink"
    );
}

/// The episode state machine's exact output, driven synchronously so the
/// wording and the position of both markers are pinned without a thread
/// in sight.
///
/// Records are handed to the writer pre-built rather than through
/// `enqueue_async_into` because the interesting sequence — shed, shed,
/// recovered, shed again — needs the carried counts chosen, and a real
/// producer can only produce them by actually saturating a queue (which
/// is what the concurrent test above does).
///
/// Un-fixed behaviour: a marker before `gap opens` AND before `still
/// shedding`, and no summary at all when the queue recovers.
#[test]
fn an_overload_burst_is_summarised_once_when_the_queue_catches_up() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 64;
    let clear_run = crate::diag::ASYNC_BURST_CLEAR_RUN;

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);

    let mut queued: Vec<(u64, String)> = vec![
        (0, "before".to_owned()),
        (3, "gap opens".to_owned()),
        (4, "still shedding".to_owned()),
    ];
    queued.extend((0..clear_run).map(|i| (0, format!("recovered #{i}"))));
    queued.push((2, "second burst".to_owned()));
    // The ledger has to agree with the hand-built records: a real
    // producer that bound 3 + 4 + 2 drops to these records would have
    // counted all nine as shed-but-unnamed first. Stating it here keeps
    // the "every shed record ends up named exactly once" invariant
    // assertable at the bottom.
    dropped.carried_for_tests(9);
    for (drops_before, message) in &queued {
        pending.fetch_add(1, Ordering::Relaxed);
        tx.send(crate::diag::AsyncRecord::Line {
            drops_before: *drops_before,
            message: message.clone(),
        })
        .expect("the capacity-64 queue accepts the whole script");
    }
    // Closed before the loop runs, so it drains and returns inline: no
    // thread to join, no parked writer for an assertion to unwind into.
    drop(tx);

    let mut recorded: Vec<String> = Vec::new();
    crate::diag::run_async_writer_loop(rx, CAPACITY, &pending, &dropped, |line| {
        recorded.push(line.to_owned());
    });

    let mut expected: Vec<String> = vec![
        "before".to_owned(),
        // The episode opens naming only what THIS record carried...
        crate::diag::async_dropped_marker(3, CAPACITY),
        "gap opens".to_owned(),
        // ...and the next shed is folded in silently.
        "still shedding".to_owned(),
    ];
    expected.extend((0..clear_run - 1).map(|i| format!("recovered #{i}")));
    // `clear_run` clean records in a row means the queue caught up, so the
    // episode closes with ONE summary naming 3 + 4.
    expected.push(crate::diag::async_burst_summary_marker(7, CAPACITY));
    expected.push(format!("recovered #{}", clear_run - 1));
    // A fresh gap after the recovery is a NEW episode, announced again.
    expected.push(crate::diag::async_dropped_marker(2, CAPACITY));
    expected.push("second burst".to_owned());
    // Disconnected with an episode still open: it is closed and summarised
    // rather than left unreported.
    expected.push(crate::diag::async_burst_summary_marker(2, CAPACITY));

    assert_eq!(
        recorded, expected,
        "one marker opens an overload episode, one summary closes it once \
         {clear_run} consecutive records arrive with nothing shed ahead of \
         them, and a later gap opens a new episode. Anything else is either \
         per-record amplification or a lost count."
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "every record must still be counted out of `pending` after it is \
         written"
    );
    assert_eq!(
        dropped.unbound(),
        0,
        "closing an episode must reset the shared counter"
    );
    assert_eq!(
        dropped.unnamed(),
        0,
        "each of the 9 carried drops must be named exactly once - a \
         residue is an unreported gap, a negative-turned-saturated ledger \
         would be a double report"
    );
}

/// The interaction between #680's [`BurstState`] and this PR's drain:
/// an exit that lands MID-EPISODE must still write the episode summary.
///
/// A burst is announced once when it opens and summarised once when the
/// queue catches up. A drain arriving before the queue caught up is the
/// LAST thing that will ever happen on this writer, so if the shutdown
/// arm just acks and returns, the episode's closing summary is never
/// written and the tee file's final word about a wedged sink is the
/// episode's OPENING count instead of its total - on precisely the
/// crash-adjacent exit both mechanisms exist to make readable.
///
/// Deterministic and inline: records are handed to the writer pre-built
/// so the carried counts are chosen rather than raced for, and the
/// sentinel is already in the channel before the loop starts, so there
/// is no thread to join and no timing to assert.
///
/// Un-fixed behaviour (drain arm without
/// `close_burst_with_pending_drops`): the summary line is missing and
/// the outstanding counter is left non-zero.
#[test]
fn a_drain_landing_mid_burst_still_writes_the_episode_summary() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 16;
    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);

    // One record carrying a shed count opens the episode...
    pending.fetch_add(1, Ordering::Relaxed);
    tx.send(crate::diag::AsyncRecord::Line {
        drops_before: 7,
        message: "after the gap".to_owned(),
    })
    .expect("the queue accepts the opening record");
    // ...and the drain arrives while it is still open, far short of the
    // ASYNC_BURST_CLEAR_RUN clean records that would close it normally.
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
    tx.send(crate::diag::AsyncRecord::Shutdown(ack_tx))
        .expect("the queue accepts the sentinel");
    // Plus a shed that no record will ever carry - the drain is the last
    // chance to report it. `carried_for_tests(7)` is the other half of
    // the ledger state a real producer would have left behind for the
    // hand-built record above.
    dropped.carried_for_tests(7);
    dropped.shed_for_tests(3);

    let mut recorded: Vec<String> = Vec::new();
    crate::diag::run_async_writer_loop(rx, CAPACITY, &pending, &dropped, |line| {
        recorded.push(line.to_owned());
    });

    assert_eq!(
        recorded,
        vec![
            crate::diag::async_dropped_marker(7, CAPACITY),
            "after the gap".to_owned(),
            crate::diag::async_burst_summary_marker(10, CAPACITY),
        ],
        "the shutdown arm must close the open episode, folding in the 3 \
         records no accepted record could carry, so the tee file names \
         the episode TOTAL before the process goes away"
    );
    assert!(
        ack_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok(),
        "the drain must still be acknowledged after the summary is written"
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "the drained record must still be counted out of `pending`"
    );
    assert_eq!(
        dropped.unbound(),
        0,
        "closing the episode on the drain must reset the shared counter"
    );
    assert_eq!(
        dropped.unnamed(),
        0,
        "the drain is the last chance to name a gap; nothing may be left \
         outstanding on the ledger once the writer has acknowledged"
    );
}

/// A healthy, drained pipeline must report nothing shed. Names
/// `async_dropped_count` so the accessor has a test that exercises it,
/// and pins the other half of the contract: the marker is an anomaly
/// signal, so ordinary traffic through the real process-wide writer
/// must never produce one.
#[test]
fn async_dropped_count_is_zero_on_a_healthy_drained_pipeline() {
    let _guard = diag_test_lock();
    crate::diag::ensure_async_writer();
    crate::diag::log_async!("[test] healthy async pipeline line");
    crate::diag::flush_async_for_tests();
    assert_eq!(
        crate::diag::async_dropped_count(),
        0,
        "a handful of records through an idle capacity-{} queue must not \
         shed anything; a non-zero count here means the accounting is \
         counting successful sends",
        crate::diag::ASYNC_QUEUE_CAPACITY
    );
}

/// Structural companion to the runtime test above: the runtime test
/// drives the parameterised halves, this one pins that PRODUCTION is
/// actually wired to them. A regression that reverted `enqueue_async`
/// to `if tx.try_send(message).is_ok()`, or that spawned a writer loop
/// which never consults the drop counter, would leave the runtime test
/// green while shipping silent drops again.
#[test]
fn production_async_queue_is_wired_to_the_drop_accounting() {
    let enqueue = scan_fn_body(
        "src/rust/diag.rs",
        "pub fn enqueue_async(message: String) {",
    );
    assert!(
        enqueue.code.contains("enqueue_async_into"),
        "enqueue_async must route through `enqueue_async_into` so the \
         production path and the tested path are the same code. \
         Offending function body:\n{}",
        enqueue.raw
    );

    let delegate = scan_fn_body("src/rust/diag.rs", "pub(crate) fn enqueue_async_into(");
    assert!(
        delegate.code.contains("enqueue_async_into_after"),
        "enqueue_async_into must delegate to `enqueue_async_into_after` \
         with an empty seam, so the function `diag_tests` drives the \
         reservation-vs-sentinel race through IS the production sender \
         and not a parallel copy. Offending function body:\n{}",
        delegate.raw
    );

    let sender = scan_fn_body(
        "src/rust/diag.rs",
        "pub(crate) fn enqueue_async_into_after<H>(",
    );
    assert!(
        sender.code.contains("TrySendError::Full"),
        "enqueue_async_into must match on `TrySendError::Full` — an \
         `is_ok()` shortcut collapses `Full` and `Disconnected` into one \
         silent discard and loses the drop accounting. Offending \
         function body:\n{}",
        sender.raw
    );
    assert!(
        sender.code.contains("dropped.shed("),
        "enqueue_async_into_after must bump the drop LEDGER on a full \
         queue, otherwise the writer has nothing to report. Offending \
         function body:\n{}",
        sender.raw
    );
    assert!(
        !sender.code.contains("tx.send("),
        "enqueue_async_into must never use the BLOCKING `send` — it runs \
         on the Windows LL-hook callback thread, where parking on a slow \
         sink is the wedge this queue exists to prevent. Offending \
         function body:\n{}",
        sender.raw
    );
    assert!(
        sender.code.contains("take_unbound") && sender.code.contains("drops_before"),
        "enqueue_async_into_after must bind the outstanding shed count to \
         the record it is accepting (an `AsyncRecord::Line` carrying \
         `drops_before`), \
         not leave it for the writer to read at write time — `Full` sheds \
         the NEWEST record, so a writer-side read dates the gap ahead of \
         the older records still queued. Offending function body:\n{}",
        sender.raw
    );

    let writer = scan_fn_body(
        "src/rust/diag.rs",
        "pub(crate) fn run_async_writer_loop<F>(",
    );
    assert!(
        writer.code.contains("recv_timeout"),
        "run_async_writer_loop must park with `recv_timeout`, never a \
         plain blocking `recv()`: the process-wide sender is never \
         dropped, so a writer parked on `recv()` never wakes to report a \
         burst shed after the last record — the wedge case the drop \
         accounting exists for. Offending function body:\n{}",
        writer.raw
    );
    assert!(
        writer.code.contains("BurstState"),
        "run_async_writer_loop must carry the overload-episode state \
         across iterations: emitting a marker for every record that \
         carries a non-zero count is nearly one marker per surviving \
         record under a sustained overload, which doubles the write \
         volume against the sink that was already too slow (Codex P2 \
         #680 comment 3668174780). Offending function body:\n{}",
        writer.raw
    );

    let install = scan_fn_body("src/rust/diag.rs", "pub fn ensure_async_writer() {");
    assert!(
        install.code.contains("run_async_writer_loop"),
        "the production writer thread must run `run_async_writer_loop` \
         so it emits the coalesced dropped marker; an inline `while let \
         Ok(line) = rx.recv()` loop would silently skip the accounting. \
         Offending function body:\n{}",
        install.raw
    );
    assert!(
        install.code.contains("ASYNC_DROPPED"),
        "the production writer thread must be handed the process-wide \
         drop counter that `enqueue_async` bumps; handing it a different \
         counter would report zero forever. Offending function body:\n{}",
        install.raw
    );
}

// -----------------------------------------------------------------------
// Drain-on-exit for the off-callback async queue.
//
// The writer is a background thread. On process exit it is killed
// without draining, so whatever is still queued is lost - including, on
// a crash-adjacent exit, the very records a support thread needs.
// `drain_and_shutdown` pushes a `Shutdown` sentinel through the SAME
// bounded queue as the records (so everything enqueued before the call
// is necessarily ahead of it), the writer flushes and acks, and the
// caller gives up after a bounded deadline so a wedged sink cannot pin
// process teardown.
// -----------------------------------------------------------------------

/// The drain must flush a backlog that was queued while the sink was
/// parked, and only then acknowledge.
///
/// Un-fixed behaviour (a writer that treats the sentinel as "stop now"
/// instead of "drain, then stop", or no sentinel at all): the ack
/// arrives with records still unwritten, so the sink is short and the
/// tee file loses the tail of the capture.
#[test]
fn drain_and_shutdown_flushes_the_queued_backlog_before_acknowledging() {
    // The pipeline halves under test are parameterised away from the
    // global writer slot and the LEVEL atomic, but a future rework that
    // reached for either would otherwise flake against the rest of the
    // suite.
    let _guard = diag_test_lock();

    const CAPACITY: usize = 64;
    const RECORDS: usize = 32;

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);
    let sink: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let gate: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

    // NOTHING is asserted inside the scope: a panic there unwinds with
    // the writer still parked on the gate, and `thread::scope` would
    // then block forever trying to join it - turning an expected FAILURE
    // into an unkillable HANG. Observe inside, assert outside.
    let (acked, second_elapsed, elapsed) = std::thread::scope(|scope| {
        let (pending_ref, dropped_ref) = (&pending, &dropped);
        let (sink_ref, gate_ref) = (&sink, &gate);
        scope.spawn(move || {
            let mut stalled_once = false;
            crate::diag::run_async_writer_loop(rx, CAPACITY, pending_ref, dropped_ref, |line| {
                if !stalled_once {
                    stalled_once = true;
                    let (lock, cv) = gate_ref;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = cv.wait(released).unwrap();
                    }
                }
                sink_ref.lock().unwrap().push(line.to_owned());
            });
        });

        // Build a real backlog: the writer parks on its first record, so
        // all of these sit in the queue (well inside CAPACITY, so
        // nothing is shed).
        for i in 0..RECORDS {
            crate::diag::enqueue_async_into(
                &tx,
                &admission,
                &pending,
                &dropped,
                format!("record #{i}"),
            );
        }

        // Release the sink only AFTER the drain has started, so the
        // drain genuinely waits on a backlog rather than on an
        // already-idle writer.
        let gate_releaser = &gate;
        scope.spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let (lock, cv) = gate_releaser;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        });

        let started = std::time::Instant::now();
        let acked = crate::diag::drain_and_shutdown_into(
            &tx,
            &admission,
            std::time::Duration::from_secs(10),
        );
        let elapsed = started.elapsed();
        // A second drain against a writer that already stopped must
        // return PROMPTLY rather than sit out the deadline: teardown can
        // run twice on a nested error path. Whether it reports success
        // (`Disconnected` on the way in) or failure (the ack sender died
        // with the receiver) is a race with the writer thread's final
        // drop, and either answer is honest - the stall is the bug.
        let second_started = std::time::Instant::now();
        let _ = crate::diag::drain_and_shutdown_into(
            &tx,
            &admission,
            std::time::Duration::from_secs(10),
        );
        let second_elapsed = second_started.elapsed();
        (acked, second_elapsed, elapsed)
    });

    assert!(
        acked,
        "the writer must acknowledge the drain; false here means the \
         sentinel never reached it and every queued record would be lost \
         at process exit"
    );
    assert!(
        second_elapsed < std::time::Duration::from_secs(5),
        "draining an already-stopped writer must return promptly, not sit \
         out the whole deadline; took {second_elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the drain must return as soon as the writer acks; took {elapsed:?}"
    );

    let recorded = sink.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        RECORDS,
        "every record queued before the drain must reach the sink BEFORE \
         the ack; got {} of {RECORDS}: {recorded:?}",
        recorded.len()
    );
    assert_eq!(
        recorded.first().map(String::as_str),
        Some("record #0"),
        "the backlog must be flushed in order; got {recorded:?}"
    );
    assert_eq!(
        recorded.last().map(String::as_str),
        Some(format!("record #{}", RECORDS - 1)).as_deref(),
        "the LAST record queued before exit is the one a wedge repro \
         needs most; got {recorded:?}"
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "a completed drain must leave nothing pending"
    );
}

/// The drain must be BOUNDED: a writer wedged inside its sink with a
/// full queue cannot park process teardown.
///
/// Un-fixed behaviour (a blocking `tx.send(sentinel)` or an unbounded
/// `ack_rx.recv()`): this call never returns and the process hangs on
/// exit instead of losing a few log lines - strictly worse than the bug
/// the drain was added to fix.
#[test]
fn drain_and_shutdown_gives_up_on_a_wedged_writer_within_the_deadline() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const DEADLINE: std::time::Duration = std::time::Duration::from_millis(80);

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);
    let gate: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

    // Observe inside, assert outside (see the sibling test).
    let (acked, elapsed) = std::thread::scope(|scope| {
        let (pending_ref, dropped_ref, gate_ref) = (&pending, &dropped, &gate);
        scope.spawn(move || {
            let mut stalled_once = false;
            crate::diag::run_async_writer_loop(rx, CAPACITY, pending_ref, dropped_ref, |_line| {
                if !stalled_once {
                    stalled_once = true;
                    let (lock, cv) = gate_ref;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = cv.wait(released).unwrap();
                    }
                }
            });
        });

        // Wedge the writer and fill the queue so the sentinel cannot
        // even be handed over - this is the `try_send` polling path.
        for i in 0..(CAPACITY * 4) {
            crate::diag::enqueue_async_into(
                &tx,
                &admission,
                &pending,
                &dropped,
                format!("wedge #{i}"),
            );
        }

        let started = std::time::Instant::now();
        let acked = crate::diag::drain_and_shutdown_into(&tx, &admission, DEADLINE);
        let elapsed = started.elapsed();

        // Let the writer finish so `thread::scope` can join it.
        {
            let (lock, cv) = &gate;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        drop(tx);
        (acked, elapsed)
    });

    assert!(
        !acked,
        "a wedged writer must report an INCOMPLETE drain so the caller \
         can warn the operator that the tee file may be short"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "the drain must be bounded by its deadline ({DEADLINE:?}); a \
         wedged writer pinned teardown for {elapsed:?}"
    );
}

/// Codex P2 #681 PRRT_kwDOSfNjQs6UiJ_T - the shutdown sweep must not be
/// extended by traffic queued AFTER the sentinel.
///
/// The real scenario: the Windows `WH_KEYBOARD_LL` / rdev callback
/// thread is unjoinable, so it keeps producing records all through
/// teardown - the documented high-rate mouse trace is the case that
/// motivated the queue in the first place. Everything the drain request
/// covered is ordered ahead of the sentinel and therefore already
/// written by the time the writer sees it; anything the sweep pulls
/// afterwards is younger traffic that no `main` on its way out is
/// waiting for. If the sweep follows that traffic, the caller sits out
/// its whole deadline and reports a FAILED drain - warning the operator
/// that the tee file is short - on a run where nothing was lost at all.
///
/// ## The deterministic seam (no sleeps, no scheduling race)
///
/// The producer IS the sink: every line the writer emits immediately
/// re-enqueues one record on the writer's own thread, so each dequeue
/// is replaced before the next `try_recv` runs and the queue provably
/// never runs dry. That is exactly the "producer keeps up with the
/// sink" steady state, reproduced without a second thread to race
/// against. `CAPACITY` (4) is never approached - the steady-state depth
/// is one record plus the sentinel - so nothing is shed and no marker
/// perturbs the count.
///
/// Un-fixed behaviour (`while let Ok(queued) = rx.try_recv()` with no
/// budget): the sweep never sees an empty queue, the ack never arrives,
/// and this fails on "the drain must be acknowledged".
#[test]
fn drain_and_shutdown_is_not_extended_by_traffic_queued_after_the_sentinel() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const DEADLINE: std::time::Duration = std::time::Duration::from_millis(250);

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);
    let feed = tx.clone();
    let keep_feeding = std::sync::atomic::AtomicBool::new(true);

    // Observe inside, assert outside: a panic in the scope would unwind
    // while the writer is still sweeping, and `thread::scope` would then
    // block forever joining it - an expected FAILURE turned into a HANG.
    let (acked, elapsed) = std::thread::scope(|scope| {
        let (pending_ref, dropped_ref) = (&pending, &dropped);
        let feeding_ref = &keep_feeding;
        scope.spawn(move || {
            crate::diag::run_async_writer_loop(rx, CAPACITY, pending_ref, dropped_ref, |_line| {
                if feeding_ref.load(Ordering::Relaxed) {
                    // One record out, one record in - the callback
                    // thread that will not stop firing during teardown.
                    pending_ref.fetch_add(1, Ordering::Relaxed);
                    let _ = feed.try_send(crate::diag::AsyncRecord::Line {
                        drops_before: 0,
                        message: "post-sentinel callback trace".to_owned(),
                    });
                }
            });
        });

        // Prime the self-sustaining feed; from here the queue is never
        // empty again until `keep_feeding` is cleared.
        crate::diag::enqueue_async_into(&tx, &admission, &pending, &dropped, "seed".to_owned());

        let started = std::time::Instant::now();
        let acked = crate::diag::drain_and_shutdown_into(&tx, &admission, DEADLINE);
        let elapsed = started.elapsed();

        // Let the writer out so `thread::scope` can join it: on the
        // un-fixed tree it is still chasing its own feed.
        keep_feeding.store(false, Ordering::Relaxed);
        (acked, elapsed)
    });

    assert!(
        acked,
        "the drain must be acknowledged even while a producer keeps \
         feeding the queue: every record the request covered was ordered \
         AHEAD of the sentinel and is already written, so following the \
         younger traffic only burns the caller's {DEADLINE:?} budget and \
         reports a lost-records warning on a run that lost nothing"
    );
    assert!(
        elapsed < DEADLINE,
        "an acknowledged drain must return well inside its deadline; a \
         sweep dragged along by post-sentinel traffic took {elapsed:?} of \
         {DEADLINE:?}"
    );
}

/// The post-drain warning path must never wait on the tee-file mutex.
///
/// This is the exact deadlock the deadline exists to avoid: the
/// likeliest reason a drain timed out is that the writer thread is
/// parked INSIDE `write_line_to` holding this mutex, so a blocking
/// `log!` in the timeout branch would queue behind the wedged writer and
/// hang teardown forever.
///
/// Un-fixed behaviour (`write_line`'s blocking `diag_file().lock()`):
/// this test deadlocks on its own held guard instead of returning
/// `false`.
#[test]
fn write_line_nonblocking_skips_the_tee_when_the_mutex_is_contended() {
    let _guard = diag_test_lock();
    // The sink short-circuits at `Off`; make sure a previous test's
    // level choice cannot mask the contract under test.
    reset_level_for_tests();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nonblocking.log");
    install_gui_diagnostic_log(&path).expect("install nonblocking sink");

    // Uncontended: the tee write is attempted and lands.
    assert!(
        crate::diag::write_line_nonblocking("[test] uncontended nonblocking line"),
        "an uncontended tee mutex must still produce the file write - the \
         non-blocking variant is a fallback, not a stderr-only sink"
    );

    let started = std::time::Instant::now();
    let attempted_tee = {
        // Hold the very mutex a wedged writer would be holding.
        let _held = crate::diag::tee_mutex_for_tests().lock().unwrap();
        crate::diag::write_line_nonblocking("[test] contended nonblocking line")
    };
    let elapsed = started.elapsed();

    assert!(
        !attempted_tee,
        "with the tee mutex held, the line must go to stderr only; true \
         here means the write blocked on (or bypassed) the mutex"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "write_line_nonblocking must `try_lock`, never `lock`; the \
         contended call took {elapsed:?}"
    );

    let contents = std::fs::read_to_string(&path).expect("read tee file");
    assert!(
        contents.contains("[test] uncontended nonblocking line"),
        "the uncontended write must be in the tee file; got {contents:?}"
    );
    assert!(
        !contents.contains("[test] contended nonblocking line"),
        "the contended write must have been SKIPPED, not queued behind the \
         mutex; got {contents:?}"
    );
}

/// Codex P2 #681 PRRT_kwDOSfNjQs6UfWDz - an unexpected writer
/// disconnect is a drain FAILURE, not a success.
///
/// Reachable in production: `ensure_async_writer` deliberately swallows
/// a thread-spawn error, so the sender can be installed with no writer
/// behind it at all; a writer that panicked drops its receiver the same
/// way. Either way the sentinel never reaches anybody, nothing
/// acknowledges the drain, and whatever was queued dies with the
/// process. Reporting `true` there suppresses the caller's exit warning
/// on exactly the runs where diagnostics WERE lost.
///
/// Fully deterministic - no threads, no timing: the receiver is dropped
/// before the call, so `try_send` reports `Disconnected` on its first
/// attempt.
///
/// Un-fixed behaviour (`Err(TrySendError::Disconnected(_)) => true`):
/// panicked "a drain whose writer is GONE must report failure".
#[test]
fn drain_and_shutdown_reports_failure_when_the_writer_is_disconnected() {
    let _guard = diag_test_lock();

    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(4);
    // Whatever is still queued dies here, exactly as it would if the
    // writer thread had panicked or never spawned.
    drop(rx);

    let started = std::time::Instant::now();
    let acked = crate::diag::drain_and_shutdown_into(
        &tx,
        &admission,
        std::time::Duration::from_millis(500),
    );
    let elapsed = started.elapsed();

    assert!(
        !acked,
        "a drain whose writer is GONE must report failure: nothing \
         acknowledged the shutdown and every queued record was lost, so \
         `true` here silently suppresses the exit warning that tells the \
         operator the tee file may be short"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "a disconnected channel must be detected immediately, not waited \
         out for the whole deadline; took {elapsed:?}"
    );
}

/// Codex P1 #681 PRRT_kwDOSfNjQs6UfWDv - the PRODUCTION lock ordering,
/// not just the tee mutex held from the test thread.
///
/// `write_line_nonblocking_skips_the_tee_when_the_mutex_is_contended`
/// above holds `diag_file()` directly, so it proves the `try_lock` but
/// says nothing about the OTHER lock production takes. The real wedge
/// has two locks in it: the async writer thread is inside
/// `write_line_to`, and `write_line` handed it the process
/// `std::io::stderr()` guard. If `write_line_to` keeps that guard across
/// its blocking `diag_file().lock()`, a wedged AppData volume pins the
/// process stderr lock as well - and then `write_line_nonblocking`
/// blocks on `stderr.lock()` and NEVER REACHES its `try_lock`. The
/// 500 ms `DIAG_DRAIN_DEADLINE` buys nothing: teardown hangs on the
/// stderr lock instead of the tee mutex, which is exactly what the
/// non-blocking sink was added to prevent.
///
/// So this reproduces the production ordering: a thread takes the real
/// stderr guard and hands it to `write_line_to` (the same call
/// `write_line` makes) while this thread holds the tee mutex, then a
/// second thread runs the teardown-warning path.
///
/// Deterministic seam, no sleeps: the wedger announces itself only
/// AFTER it holds the stderr lock, so by the time the probe starts the
/// contended state provably exists. Every rendezvous receive is bounded,
/// so a harness bug asserts instead of hanging.
///
/// Nothing panics while `held` is alive: unwinding through a live
/// `MutexGuard` would POISON the process-wide tee mutex and silently
/// disable the file write for every later test in this binary.
///
/// Un-fixed behaviour (`write_line_to(&mut stderr.lock(), ...)`, guard
/// alive across the tee lock): the probe never returns and this test
/// fails on the bounded receive.
#[test]
fn a_wedged_tee_write_does_not_pin_the_stderr_lock_against_the_teardown_warning() {
    let _guard = diag_test_lock();
    reset_level_for_tests();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("lock-ordering.log");
    install_gui_diagnostic_log(&path).expect("install lock-ordering sink");

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let (probe_tx, probe_rx) = std::sync::mpsc::channel::<bool>();

    // Hold the lock a wedged AppData volume would be holding.
    let held = crate::diag::tee_mutex_for_tests().lock().unwrap();

    // The "async writer thread": production ordering, verbatim.
    std::thread::spawn(move || {
        let stderr_guard = std::io::stderr().lock();
        // The token is the seam: it is sent while the stderr lock is
        // provably held, so the probe below cannot start early.
        let _ = ready_tx.send(());
        crate::diag::write_line_to(stderr_guard, "t=0ms [test] wedged writer line");
    });

    let wedger_ready = ready_rx.recv_timeout(std::time::Duration::from_secs(10));

    // The teardown warning path, on its own thread so a regression
    // blocks IT rather than the test body.
    std::thread::spawn(move || {
        let attempted =
            crate::diag::write_line_nonblocking("[test] teardown warning past the wedge");
        let _ = probe_tx.send(attempted);
    });

    let probed = probe_rx.recv_timeout(std::time::Duration::from_secs(5));

    // Release the wedge BEFORE any assertion can unwind.
    drop(held);

    assert!(
        wedger_ready.is_ok(),
        "harness: the wedging thread never reported holding the stderr \
         lock, so nothing about the lock ordering was exercised"
    );
    assert_eq!(
        probed,
        Ok(false),
        "the teardown warning must reach its `try_lock` and report \
         `false` (stderr only) while the tee mutex is wedged. An Err \
         here means it never returned at all - it blocked acquiring the \
         process stderr lock that `write_line_to` was still holding \
         across its blocking tee write, so a wedged AppData sink pins \
         process exit past DIAG_DRAIN_DEADLINE. Release the stderr \
         guard before the tee write."
    );

    // The wedger must still complete its tee write once unblocked -
    // releasing stderr early must not have cost the file record.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut contents = String::new();
    while std::time::Instant::now() < deadline {
        contents = std::fs::read_to_string(&path).unwrap_or_default();
        if contents.contains("[test] wedged writer line") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        contents.contains("[test] wedged writer line"),
        "the wedged writer must still land its tee record once the mutex \
         is free; releasing the stderr guard early must not drop the \
         file write. Tee contents: {contents:?}"
    );
    assert!(
        !contents.contains("[test] teardown warning past the wedge"),
        "the teardown warning must have SKIPPED the tee, not queued \
         behind the wedge. Tee contents: {contents:?}"
    );
}

/// A tee sink whose mutex is perfectly acquirable and whose `write`
/// NEVER RETURNS until the test says so.
///
/// This is the shape Codex P1 #681 PRRT_kwDOSfNjQs6UjZeP names and the
/// one no `tempfile` can produce: a stalled AppData volume does not hold
/// the `Mutex`, it holds the *syscall*. `try_lock` bounds the former and
/// says nothing about the latter, which is why the exit-teardown warning
/// had to stop touching the tee altogether rather than merely stop
/// blocking on its lock.
struct BlockedTee {
    gate: std::sync::Arc<(Mutex<bool>, Condvar)>,
}

impl std::io::Write for BlockedTee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let (lock, cv) = &*self.gate;
        let mut released = lock.lock().unwrap_or_else(|p| p.into_inner());
        while !*released {
            released = cv.wait(released).unwrap_or_else(|p| p.into_inner());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Release a [`BlockedTee`]'s gate so the stalled write can complete.
fn release_gate(gate: &(Mutex<bool>, Condvar)) {
    let (lock, cv) = gate;
    *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
    cv.notify_all();
}

/// Codex P1 #681 PRRT_kwDOSfNjQs6UjZeP, half one: with the tee mutex
/// FREE, the exit-teardown timeout warning must not reach the tee file
/// at all.
///
/// The predecessor sink (`write_line_nonblocking`) only `try_lock`s. A
/// free mutex - a writer thread that disconnected, or one that released
/// the mutex a microsecond before teardown ran - therefore hands it a
/// SUCCESSFUL lock and it goes on to do a synchronous `writeln!` +
/// `flush` on the same volume that just failed to drain.
///
/// Fully deterministic: no threads, no timing. The tee file's contents
/// are the observation, and the control line proves the tee was live and
/// uncontended for the duration.
///
/// Un-fixed behaviour (`exit_timeout_warning_sink` calling
/// `crate::diag::write_line_nonblocking`): the warning IS in the tee
/// file and this fails on "must not have reached the tee file".
#[test]
fn the_exit_timeout_warning_never_reaches_a_free_tee() {
    let _guard = diag_test_lock();
    // The sink short-circuits at `Off`; a previous test's level choice
    // must not be able to mask the contract under test.
    reset_level_for_tests();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("timeout-warning.log");
    install_gui_diagnostic_log(&path).expect("install timeout-warning sink");

    // Control: the tee is installed, live, and its mutex is free.
    crate::diag::log!("[test] tee is live and uncontended");

    let completed = crate::entrypoint::drain_diagnostics_on_exit_with(
        |_deadline| false,
        crate::entrypoint::exit_timeout_warning_sink,
        crate::entrypoint::DIAG_DRAIN_DEADLINE,
    );

    let contents = std::fs::read_to_string(&path).expect("read tee file");
    // Put the process-wide slot back before anything can unwind.
    crate::diag::install_tee_sink_for_tests(None);

    assert!(
        !completed,
        "harness: the injected drain must report a timeout"
    );
    assert!(
        contents.contains("[test] tee is live and uncontended"),
        "harness: the tee was not actually live, so the absence of the \
         warning below proves nothing. Tee contents: {contents:?}"
    );
    assert!(
        !contents.contains(crate::entrypoint::DIAG_DRAIN_TIMEOUT_WARNING),
        "the exit-teardown timeout warning must not have reached the tee \
         file. A `try_lock` succeeds whenever the mutex is free, and the \
         file write behind it is unbounded on the very volume that just \
         failed to drain - so process exit hangs inside the warning about \
         the wedged sink, past DIAG_DRAIN_DEADLINE. Tee contents: \
         {contents:?}"
    );
}

/// Codex P1 #681 PRRT_kwDOSfNjQs6UjZeP, half two: the FREE-MUTEX /
/// BLOCKED-WRITE case, which is the one the previous round's test could
/// not express.
///
/// `write_line_nonblocking_skips_the_tee_when_the_mutex_is_contended`
/// HOLDS the mutex, so it exercises only the `try_lock` miss. Here the
/// mutex is never held by anyone: the warning acquires it on the first
/// attempt and then parks forever inside the sink's `write`. `try_lock`
/// bounds lock acquisition, not the file I/O behind it.
///
/// Every wait is bounded, so a harness bug asserts instead of hanging,
/// and the gate is released (and the process-wide slot restored) BEFORE
/// any assertion can unwind - a live `MutexGuard` unwound through would
/// poison the tee mutex for every later test in this binary.
///
/// Un-fixed behaviour (`exit_timeout_warning_sink` calling
/// `crate::diag::write_line_nonblocking`): the warning thread never
/// returns and this fails on "the exit-teardown timeout warning must
/// return while the tee sink is stalled".
#[test]
fn the_exit_timeout_warning_does_not_write_to_a_free_but_blocked_tee() {
    let _guard = diag_test_lock();
    reset_level_for_tests();

    let gate = std::sync::Arc::new((Mutex::new(false), Condvar::new()));
    crate::diag::install_tee_sink_for_tests(Some(Box::new(BlockedTee {
        gate: std::sync::Arc::clone(&gate),
    })));

    let (done_tx, done_rx) = std::sync::mpsc::channel::<bool>();
    let warner = std::thread::spawn(move || {
        // The production wiring, verbatim, with only the drain result
        // forced: a real `drain_and_shutdown` here would stop the
        // process-wide writer thread for every later test.
        let completed = crate::entrypoint::drain_diagnostics_on_exit_with(
            |_deadline| false,
            crate::entrypoint::exit_timeout_warning_sink,
            crate::entrypoint::DIAG_DRAIN_DEADLINE,
        );
        let _ = done_tx.send(completed);
    });

    let returned = done_rx.recv_timeout(std::time::Duration::from_secs(5));

    // Unwedge, reap, restore - all before any assertion can unwind.
    release_gate(&gate);
    let joined = warner.join();
    crate::diag::install_tee_sink_for_tests(None);

    assert_eq!(
        returned,
        Ok(false),
        "the exit-teardown timeout warning must return while the tee sink \
         is stalled. An Err here means it never returned at all: the tee \
         mutex was FREE, so a `try_lock` succeeded and the warning then \
         blocked inside the sink's `write` - pinning process exit on the \
         wedged AppData volume it was trying to warn about, well past \
         DIAG_DRAIN_DEADLINE. Emit the warning through a sink that does \
         not touch the tee."
    );
    assert!(joined.is_ok(), "the warning thread must not have panicked");
}

/// Codex P2 #682 comment 3669770206 - the exit-teardown timeout warning
/// must not block on the PROCESS STDERR LOCK either.
///
/// This is the third instance of one shape (see
/// `crate::entrypoint::exit_timeout_warning_sink` for all three). When
/// CLI stderr is redirected to a full or stalled pipe, the async writer
/// blocks inside its `writeln!` while HOLDING `std::io::Stderr`'s lock.
/// The warning then wants the same lock, and `std::io::Stderr` has no
/// non-blocking variant to reach for - so choosing a different SINK, as
/// the previous two rounds did, cannot close this. The fix bounds the
/// WAIT instead: the write goes to a detached thread and the exiting
/// thread walks away after `DIAG_EXIT_WARNING_BUDGET`.
///
/// The wedge here is the real `std::io::stderr()` lock, taken by a thread
/// that then parks, which is exactly the state a writer blocked on a
/// stalled pipe leaves it in. The gate is released before any assertion
/// can unwind, and the probe runs on its own thread with a bounded
/// receive so a regression FAILS instead of hanging the suite.
///
/// Un-fixed behaviour (`exit_timeout_warning_sink` calling
/// `crate::diag::write_line_stderr_only` inline): the probe thread blocks
/// on `stderr.lock()` and never returns, and this fails on "must return
/// while a wedged writer holds the stderr lock".
#[test]
fn the_exit_timeout_warning_survives_a_writer_holding_the_stderr_lock() {
    let _guard = diag_test_lock();
    reset_level_for_tests();

    let gate = std::sync::Arc::new((Mutex::new(false), Condvar::new()));
    let (holding_tx, holding_rx) = std::sync::mpsc::channel::<()>();

    // The "async writer thread", blocked mid-write with the process
    // stderr lock in hand.
    let wedger = std::thread::spawn({
        let gate = std::sync::Arc::clone(&gate);
        move || {
            let _stderr_guard = std::io::stderr().lock();
            // Announced only once the lock is provably held, so the probe
            // below cannot start early.
            let _ = holding_tx.send(());
            let (lock, cv) = &*gate;
            let mut released = lock.lock().unwrap_or_else(|p| p.into_inner());
            while !*released {
                released = cv.wait(released).unwrap_or_else(|p| p.into_inner());
            }
        }
    });
    let wedger_ready = holding_rx.recv_timeout(std::time::Duration::from_secs(10));

    let (done_tx, done_rx) = std::sync::mpsc::channel::<bool>();
    let prober = std::thread::spawn(move || {
        // Production wiring, verbatim, with only the drain result forced:
        // a real `drain_and_shutdown` here would stop the process-wide
        // writer thread for every later test.
        let completed = crate::entrypoint::drain_diagnostics_on_exit_with(
            |_deadline| false,
            crate::entrypoint::exit_timeout_warning_sink,
            crate::entrypoint::DIAG_DRAIN_DEADLINE,
        );
        let _ = done_tx.send(completed);
    });
    let returned = done_rx.recv_timeout(std::time::Duration::from_secs(5));

    // Unwedge and reap BEFORE anything can unwind: a live stderr guard
    // unwound through would deadlock every later test that prints.
    release_gate(&gate);
    let wedger_joined = wedger.join();
    let prober_joined = prober.join();

    assert!(
        wedger_ready.is_ok(),
        "harness: the wedging thread never reported holding the stderr \
         lock, so nothing about the contended state was exercised"
    );
    assert_eq!(
        returned,
        Ok(false),
        "the exit-teardown timeout warning must return while a wedged \
         writer holds the process stderr lock. An Err here means it never \
         returned at all: it blocked on `std::io::stderr().lock()`, which \
         has no non-blocking form, so process exit is pinned for as long \
         as the redirected pipe stays stalled - past DIAG_DRAIN_DEADLINE, \
         inside the warning that the deadline expired. Bound the WAIT \
         (emit off-thread), not just the choice of sink."
    );
    assert!(
        wedger_joined.is_ok() && prober_joined.is_ok(),
        "neither the wedging thread nor the warning probe may panic"
    );
}

/// The budget is a bound on the WAIT, not on the write: an `emit` that
/// never returns must cost the caller the budget and no more, and it must
/// not be joined afterwards (a join would hand the pin straight back).
///
/// Un-fixed behaviour (an inline call, or a `join()` on the emitter):
/// this never returns and fails on the bounded receive.
#[test]
fn a_never_returning_warning_costs_the_exiting_thread_only_its_budget() {
    let gate = std::sync::Arc::new((Mutex::new(false), Condvar::new()));
    const BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

    let (done_tx, done_rx) = std::sync::mpsc::channel::<bool>();
    let caller = std::thread::spawn({
        let gate = std::sync::Arc::clone(&gate);
        move || {
            let emitted = crate::entrypoint::emit_warning_off_thread(
                move || {
                    let (lock, cv) = &*gate;
                    let mut released = lock.lock().unwrap_or_else(|p| p.into_inner());
                    while !*released {
                        released = cv.wait(released).unwrap_or_else(|p| p.into_inner());
                    }
                },
                BUDGET,
            );
            let _ = done_tx.send(emitted);
        }
    });

    let returned = done_rx.recv_timeout(std::time::Duration::from_secs(5));
    release_gate(&gate);
    let joined = caller.join();

    assert_eq!(
        returned,
        Ok(false),
        "a warning that never completes must be reported as not emitted \
         and must not detain the caller. An Err here means the caller was \
         still waiting after 5s of a {BUDGET:?} budget - the emitter was \
         joined, or run inline, either of which pins process exit on \
         whatever the sink is blocked on"
    );
    assert!(joined.is_ok(), "the calling thread must not have panicked");
}

/// The other half: a healthy sink must still produce the warning, and
/// promptly. A bound that always gives up would satisfy the test above
/// while silently deleting the diagnostic.
#[test]
fn a_healthy_warning_is_still_emitted_within_the_budget() {
    let seen = std::sync::Arc::new(Mutex::new(false));
    let emitted = crate::entrypoint::emit_warning_off_thread(
        {
            let seen = std::sync::Arc::clone(&seen);
            move || *seen.lock().unwrap_or_else(|p| p.into_inner()) = true
        },
        crate::entrypoint::DIAG_EXIT_WARNING_BUDGET,
    );
    assert!(
        emitted,
        "a warning whose sink returns immediately must be reported as \
         emitted; `false` here means the bound is swallowing the \
         diagnostic on healthy runs too"
    );
    assert!(
        *seen.lock().unwrap_or_else(|p| p.into_inner()),
        "the emitter closure must actually have run - the off-thread hop \
         must not turn the warning into a no-op"
    );
}

/// Codex P2 #681 comment 3669249174 - the drain ack must not wait on
/// traffic queued AFTER the sentinel.
///
/// The previous round bounded the formerly infinite sweep by COUNT
/// (`capacity`). A count is the wrong currency for a deadline: against a
/// slow-but-functional sink, a queue's worth of post-sentinel records
/// can cost far more than the caller's 500 ms, so a `main` on its way
/// out times out and warns the operator that the tee file is short - on
/// a run where every record the request covered was already durable
/// (FIFO puts them ahead of the sentinel). The fix acks first and sweeps
/// afterwards, on borrowed time.
///
/// ## The deterministic seam (no sleeps, no scheduling race)
///
/// The whole queue is built BEFORE the writer thread starts, so the
/// ordering under test is chosen rather than raced for:
///
/// `[pre-sentinel record] [Shutdown] [post-sentinel x5] [Shutdown]`
///
/// The sink writes the first line instantly and then parks forever. So
/// "the ack arrived" can only mean "the ack did not wait for a single
/// post-sentinel write" - a slow sink modelled as an infinitely slow one,
/// which is the same claim without a sleep in it.
///
/// The trailing second sentinel pins what the sweep is still FOR: a
/// concurrent drainer must still be acked, or its `recv_timeout` reports
/// a spurious failure.
///
/// Un-fixed behaviour (sweep-then-ack, with any budget): the sweep's
/// first post-sentinel write parks on the gate, the ack never arrives,
/// and this fails on "the drain must be acknowledged without waiting for
/// post-sentinel traffic".
#[test]
fn the_drain_ack_does_not_wait_for_post_sentinel_traffic() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 8;
    const DEADLINE: std::time::Duration = std::time::Duration::from_millis(250);

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);

    let queue_line = |message: String| {
        pending.fetch_add(1, Ordering::Relaxed);
        tx.send(crate::diag::AsyncRecord::Line {
            drops_before: 0,
            message,
        })
        .expect("the pre-built queue has room");
    };

    // The only record the drain request covers.
    queue_line("pre-sentinel record".to_owned());
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
    tx.send(crate::diag::AsyncRecord::Shutdown(ack_tx))
        .expect("the pre-built queue has room for the sentinel");
    // The unjoinable callback thread that keeps firing through teardown.
    for i in 0..(CAPACITY - 3) {
        queue_line(format!("post-sentinel callback trace #{i}"));
    }
    // A second concurrent drainer, behind all of it.
    let (second_ack_tx, second_ack_rx) = std::sync::mpsc::channel::<()>();
    tx.send(crate::diag::AsyncRecord::Shutdown(second_ack_tx))
        .expect("the pre-built queue has room for the second sentinel");
    drop(tx);

    let gate: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

    // Observe inside, assert outside: a panic in the scope would unwind
    // while the writer is parked on the gate, and `thread::scope` would
    // block forever joining it - an expected FAILURE turned into a HANG.
    let (acked, elapsed, second_acked) = std::thread::scope(|scope| {
        let (pending_ref, dropped_ref, gate_ref) = (&pending, &dropped, &gate);
        scope.spawn(move || {
            let mut written = 0usize;
            crate::diag::run_async_writer_loop(rx, CAPACITY, pending_ref, dropped_ref, |_line| {
                written += 1;
                // Line 1 is the pre-sentinel record. Everything after it
                // is post-sentinel traffic, and the sink stalls on it.
                if written > 1 {
                    let (lock, cv) = gate_ref;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = cv.wait(released).unwrap();
                    }
                }
            });
        });

        let started = std::time::Instant::now();
        let acked = ack_rx.recv_timeout(DEADLINE).is_ok();
        let elapsed = started.elapsed();

        // Let the writer out so the scope can join it: on the un-fixed
        // tree it is still parked mid-sweep.
        release_gate(&gate);
        let second_acked = second_ack_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();
        (acked, elapsed, second_acked)
    });

    assert!(
        acked,
        "the drain must be acknowledged without waiting for post-sentinel \
         traffic: every record the request covered is ordered AHEAD of the \
         sentinel and was already written, so making the caller pay for a \
         queue's worth of younger records only burns its {DEADLINE:?} \
         budget and reports a lost-records warning on a run that lost \
         nothing"
    );
    assert!(
        elapsed < DEADLINE,
        "the ack must land well inside the caller's budget; it took \
         {elapsed:?} of {DEADLINE:?}"
    );
    assert!(
        second_acked,
        "a second concurrent drainer's sentinel must still be found by the \
         post-ack sweep and acknowledged; dropping it makes that drainer's \
         `recv_timeout` report a spurious failure"
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "the sweep must still write (and count out) the post-sentinel \
         records once the sink is moving again"
    );
}

/// Codex P2 #682 comment 3669770197 - a drop reservation taken BEFORE
/// the shutdown sentinel must be named BEFORE the drain is acknowledged.
///
/// ## The race
///
/// The rdev / raw-hook callback thread is unjoinable and keeps firing all
/// through teardown. One of those callbacks can run
/// `DropLedger::take_unbound` - emptying the counter that says "a gap
/// happened" - and only then have its record accepted, by which time
/// `drain_and_shutdown_into` has already slipped its sentinel into the
/// queue ahead of it. The shutdown arm reads an unbound counter of zero,
/// finds nothing to report, and acknowledges. `main` is entitled to exit
/// the instant that ack lands, so the post-ack sweep that would have
/// found the stranded record is not guaranteed to run: a gap that
/// happened BEFORE the drain request, and the trace line that resumed
/// after it, are lost on precisely the exit the drain exists to make
/// readable.
///
/// ## The deterministic seam (no threads, no sleeps, no timing)
///
/// [`crate::diag::enqueue_async_into_after`] runs its closure in the one
/// window that matters - after the reservation is taken, before the
/// record is offered to the channel - so the sentinel is enqueued at
/// exactly the point Codex names, by construction rather than by racing
/// for it. The writer loop then runs INLINE on this thread, so "was the
/// gap named before the ack?" is answered by a `try_recv` on the ack
/// channel from inside the sink: same thread, no window for the answer to
/// change under the observation.
///
/// Un-fixed behaviour (`close_burst_with_pending_drops` in the shutdown
/// arm, i.e. reading only the unbound counter): the marker is emitted by
/// the post-ack sweep instead, so it is recorded with `acked == true` and
/// this fails on "must be named BEFORE the drain is acknowledged".
#[test]
fn a_drop_reserved_before_the_sentinel_is_named_before_the_drain_is_acked() {
    let _guard = diag_test_lock();

    const CAPACITY: usize = 4;
    const SHED: usize = 3;

    let pending = AtomicUsize::new(0);
    let dropped = DropLedger::new();
    let admission = ShutdownGate::new();
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);

    // Fill the queue, then shed: a gap that happened BEFORE any teardown.
    for i in 0..CAPACITY {
        crate::diag::enqueue_async_into(&tx, &admission, &pending, &dropped, format!("pre #{i}"));
    }
    for i in 0..SHED {
        crate::diag::enqueue_async_into(&tx, &admission, &pending, &dropped, format!("shed #{i}"));
    }
    assert_eq!(
        (dropped.unbound(), dropped.unnamed()),
        (SHED as u64, SHED as u64),
        "harness: the flood must have shed exactly {SHED} records with \
         nothing having named them yet"
    );

    // Make room the way the writer would, so the sentinel and the
    // producer's record both fit. `pending` is corrected by hand because
    // these two never reach the sink.
    for _ in 0..2 {
        rx.try_recv().expect("harness: two records to consume");
        pending.fetch_sub(1, Ordering::Relaxed);
    }

    // THE RACE, made deterministic: the producer takes the reservation,
    // the drain queues its sentinel inside that window, and the
    // producer's record is accepted behind it.
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
    crate::diag::enqueue_async_into_after(
        &tx,
        &admission,
        &pending,
        &dropped,
        "resumed after the gap".to_owned(),
        || {
            tx.send(crate::diag::AsyncRecord::Shutdown(ack_tx))
                .expect("harness: the queue has room for the sentinel");
        },
    );
    assert_eq!(
        (dropped.unbound(), dropped.unnamed()),
        (0, SHED as u64),
        "harness: the reservation must have LEFT the unbound counter (that \
         is the whole race) while the ledger still knows the gap is unnamed"
    );

    // Inline: the shutdown arm returns from the loop, so there is no
    // thread to join and nothing can be written after the snapshot.
    let mut recorded: Vec<(String, bool)> = Vec::new();
    let mut acked = false;
    crate::diag::run_async_writer_loop(rx, CAPACITY, &pending, &dropped, |line| {
        acked = acked || ack_rx.try_recv().is_ok();
        recorded.push((line.to_owned(), acked));
    });

    let named_before_ack: Vec<&String> = recorded
        .iter()
        .filter(|(line, after_ack)| line.starts_with("[diag-async]") && !after_ack)
        .map(|(line, _)| line)
        .collect();
    assert_eq!(
        named_before_ack,
        vec![&crate::diag::async_dropped_marker(SHED as u64, CAPACITY)],
        "the gap must be named BEFORE the drain is acknowledged. The \
         reservation was taken before the sentinel was queued, so it is \
         not post-sentinel traffic the ack is allowed to skip - and `main` \
         may exit the moment the ack lands, so a marker written by the \
         post-ack sweep is a marker that may never exist. Recorded \
         (line, already-acked): {recorded:?}"
    );
    assert!(
        acked,
        "harness: the drain must have been acknowledged at all"
    );
    assert_eq!(
        (dropped.unbound(), dropped.unnamed()),
        (0, 0),
        "the ledger must be empty once the writer has stopped: a residue \
         is an unreported gap, and naming the same gap twice would show up \
         as an extra marker above"
    );
    assert_eq!(
        pending.load(Ordering::Relaxed),
        0,
        "every record the queue accepted must still be counted out of \
         `pending`, including the one the sweep found behind the sentinel"
    );
}

/// Structural companion: production must be wired to the sentinel-based
/// drain, and the writer loop must flush the backlog before it acks. A
/// regression that reverted the sentinel to "stop immediately" would
/// leave the injected-core tests in `entrypoint_tests` green while
/// shipping an exit path that still discards the queue.
#[test]
fn production_async_writer_drains_before_it_stops() {
    let drain = scan_fn_body(
        "src/rust/diag.rs",
        "pub fn drain_and_shutdown(deadline: Duration) -> bool {",
    );
    assert!(
        drain.code.contains("drain_and_shutdown_into"),
        "drain_and_shutdown must route through `drain_and_shutdown_into` \
         so production and the tested path are the same code. Offending \
         function body:\n{}",
        drain.raw
    );

    let delegate = scan_fn_body("src/rust/diag.rs", "pub(crate) fn drain_and_shutdown_into(");
    assert!(
        delegate.code.contains("drain_and_shutdown_into_after"),
        "drain_and_shutdown_into must delegate to \
         `drain_and_shutdown_into_after` with an empty seam, so the \
         function `diag_tests` drives the freed-slot-vs-producer race \
         through IS the production drain and not a parallel copy. \
         Offending function body:\n{}",
        delegate.raw
    );

    let sender = scan_fn_body(
        "src/rust/diag.rs",
        "pub(crate) fn drain_and_shutdown_into_after<H>(",
    );
    assert!(
        sender.code.contains("gate.close()"),
        "drain_and_shutdown_into_after must CLOSE the admission gate \
         before it starts polling for sentinel space. Polling alone loses \
         every freed slot to the unjoinable callback producer that keeps \
         firing through teardown, so the sentinel starves for the whole \
         deadline against a writer that was never wedged (Codex P2 #681 \
         comment 3669689764). Offending function body:\n{}",
        sender.raw
    );
    assert!(
        sender.code.contains("try_send"),
        "drain_and_shutdown_into must poll `try_send`: the queue is \
         bounded, so a blocking `send` behind a stalled writer would hang \
         process exit - the very thing the deadline exists to prevent \
         (`SyncSender::send_timeout` is still unstable). Offending \
         function body:\n{}",
        sender.raw
    );
    assert!(
        sender.code.contains("recv_timeout"),
        "drain_and_shutdown_into must wait for the ack with `recv_timeout`, \
         never a bare `recv` - a wedged writer must not pin teardown. \
         Offending function body:\n{}",
        sender.raw
    );

    let loop_body = scan_fn_body(
        "src/rust/diag.rs",
        "pub(crate) fn run_async_writer_loop<F>(",
    );
    assert!(
        loop_body.code.contains("drain_and_ack_shutdown"),
        "the writer loop's Shutdown arm must route through \
         `drain_and_ack_shutdown` so the drain semantics live in one \
         place. Offending function body:\n{}",
        loop_body.raw
    );

    let drain_arm = scan_fn_body("src/rust/diag.rs", "fn drain_and_ack_shutdown<F>(");
    assert!(
        drain_arm.code.contains("try_recv"),
        "on the Shutdown sentinel the writer must still sweep what is \
         queued behind it (`try_recv`, never `recv`) - a full-at-sentinel \
         queue and a second drainer's sentinel both live there. Since \
         Codex P2 #681 comment 3669249174 that sweep runs AFTER the ack, \
         off the caller's deadline, but it must not disappear. Offending \
         function body:\n{}",
        drain_arm.raw
    );
    assert!(
        drain_arm
            .code
            .contains("close_burst_with_every_unnamed_drop"),
        "the drain must CLOSE any open overload episode before acking \
         (PR #680's `BurstState`), and it must close it on `every unnamed \
         drop` terms rather than `pending drops` terms. Two failures ride \
         on that word: a drain landing mid-burst would take the process \
         down with the episode's summary marker unwritten, and a producer \
         holding a drop reservation across the sentinel (Codex P2 #682 \
         comment 3669770197) would leave the unbound counter reading zero \
         so the gap is never named at all. Offending function body:\n{}",
        drain_arm.raw
    );
}

// -----------------------------------------------------------------------
// Writer-spawn failure accounting.
//
// `ensure_async_writer` used to do `let _ = thread::Builder::new()...
// .spawn(...)`. On a spawn failure the sender was installed anyway, so
// `async_writer_installed()` reported `true`, `enqueue_async` kept
// filling a queue nobody was reading, and once the 256 slots were gone
// every callback-path diagnostic vanished with nothing anywhere saying
// why. A process in that state is indistinguishable from a healthy quiet
// one in `gui-diagnostic.log`, which is the worst possible failure mode
// for a file whose entire job is post-hoc diagnosis.
//
// A real `thread::Builder::spawn` failure cannot be provoked in a unit
// test, so the recording half is parameterised over the slot and the
// spawn result; the structural scanner below pins that production is
// wired to it.
// -----------------------------------------------------------------------

#[test]
fn record_writer_spawn_outcome_records_a_failed_spawn() {
    let slot: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let err = std::io::Error::other("Resource temporarily unavailable (os error 11)");
    let result = crate::diag::record_writer_spawn_outcome::<()>(&slot, Err(err));
    let msg = result.expect_err("a failed spawn must be reported to the caller");
    assert!(
        msg.contains("Resource temporarily unavailable"),
        "the OS reason must survive into the recorded message so the \
         operator can tell an fd/thread exhaustion apart from anything \
         else, got {msg}"
    );
    assert_eq!(
        slot.get(),
        Some(&msg),
        "the reason must ALSO be latched in the slot - `async_writer_result` \
         reads it long after the spawn attempt, from a different thread"
    );
}

#[test]
fn record_writer_spawn_outcome_leaves_the_slot_clear_on_success() {
    let slot: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let result = crate::diag::record_writer_spawn_outcome(&slot, Ok(()));
    assert!(result.is_ok(), "a healthy spawn must report Ok");
    assert_eq!(
        slot.get(),
        None,
        "the absence of a recorded message IS 'the writer is running'; \
         a spurious entry would fail every hotkey install"
    );
}

#[test]
fn writer_spawn_failure_message_names_the_consequence() {
    let err = std::io::Error::other("no threads left");
    let msg = crate::diag::writer_spawn_failure_message(&err);
    assert!(
        msg.starts_with("[diag-async]"),
        "the marker must share the queue's log prefix so one grep finds \
         both the drop marker and the dead-writer line, got {msg}"
    );
    assert!(
        msg.contains("no threads left"),
        "the underlying io::Error must be quoted verbatim, got {msg}"
    );
    assert!(
        msg.is_ascii(),
        "diagnostic strings reach stderr under cmd.exe on a legacy code \
         page; non-ASCII renders as mojibake, got {msg}"
    );
}

// -----------------------------------------------------------------------
// Localized OS errors must not smuggle non-ASCII (or a newline) into a
// console line. Codex P2 #682 comment 3667963198.
//
// Un-fixed behaviour: `writer_spawn_failure_message` interpolated `{err}`
// raw. `console_ascii_tests` scans source LITERALS, so it proves our
// prose is ASCII and is blind to what `{err}` expands to at runtime — and
// a `thread::Builder::spawn` error is OS-derived, rendered by
// `FormatMessageW` in the SYSTEM LOCALE. On a Danish/German/Japanese/
// Russian Windows the one line explaining why the diagnostic pipeline is
// dead becomes mojibake on a legacy-code-page cmd.exe. `Error::other`
// with an ASCII literal cannot reach that case, so these drive
// `ascii_escaped` directly with the shapes a localized OS message
// actually has.
// -----------------------------------------------------------------------

#[test]
fn ascii_escaped_passes_printable_ascii_through_untouched() {
    let plain = "Resource temporarily unavailable (os error 11)";
    assert_eq!(
        crate::diag::ascii_escaped(plain),
        plain,
        "the overwhelmingly common English case must not be disfigured - \
         an escape scheme that mangles the readable path would be reverted"
    );
}

#[test]
fn ascii_escaped_escapes_a_localized_os_error() {
    // Representative of what FormatMessageW returns for a thread-creation
    // failure on a non-English Windows: Danish, German and Russian.
    for localized in [
        "Der er ikke nok hukommelse til r\u{e5}dighed",
        "Nicht gen\u{fc}gend Speicher verf\u{fc}gbar",
        "\u{41d}\u{435}\u{434}\u{43e}\u{441}\u{442}\u{430}\u{442}\u{43e}\u{447}\u{43d}\u{43e} \u{43f}\u{430}\u{43c}\u{44f}\u{442}\u{438}",
    ] {
        let escaped = crate::diag::ascii_escaped(localized);
        assert!(
            escaped.is_ascii(),
            "a localized OS error must be reduced to ASCII before it reaches \
             a legacy-code-page console, got {escaped}"
        );
        assert!(
            escaped.contains("\\u{"),
            "the non-ASCII scalars must be escaped losslessly rather than \
             dropped, so a support thread can still recover the original \
             text from the log, got {escaped}"
        );
    }
}

#[test]
fn ascii_escaped_escapes_control_characters_so_a_record_stays_one_line() {
    // A newline inside an OS error would split one record into two and
    // break the one-line-per-record grep contract the tee file is read
    // with — a worse failure than mojibake, because the second half looks
    // like an unprefixed stray line.
    let escaped = crate::diag::ascii_escaped("first line\nsecond\ttab\r\n");
    assert!(
        !escaped.contains('\n') && !escaped.contains('\r') && !escaped.contains('\t'),
        "control characters must be escaped so one diagnostic record stays \
         on one line, got {escaped}"
    );
    assert!(escaped.is_ascii(), "result must be ASCII, got {escaped}");
    assert!(
        escaped.contains("\\u{a}"),
        "the newline must survive as a visible escape rather than be \
         dropped, got {escaped}"
    );
}

#[test]
fn writer_spawn_failure_message_sanitizes_a_localized_os_error() {
    // The production case the `Error::other`-with-an-ASCII-literal test
    // above cannot reach: an OS-derived error whose text is in the
    // system locale.
    let err = std::io::Error::other("Der er ikke nok hukommelse til r\u{e5}dighed");
    let msg = crate::diag::writer_spawn_failure_message(&err);
    assert!(
        msg.is_ascii(),
        "the whole line reaches stderr via write_line; a localized OS error \
         must not make it non-ASCII, got {msg}"
    );
    assert!(
        msg.contains("Der er ikke nok hukommelse til r\\u{e5}dighed"),
        "the localized reason must still be recoverable from the log, got {msg}"
    );
    assert!(
        msg.contains("cannot be written for the rest of this process"),
        "sanitizing must not cost the consequence half of the message, got {msg}"
    );
}

/// The healthy process-wide pipeline must report a live writer. Names
/// `async_writer_result` so the accessor the rdev listener depends on
/// has a test exercising it, and pins that the new check does not
/// spuriously fail an ordinary install.
#[test]
fn async_writer_result_is_ok_on_a_healthy_process() {
    let _guard = diag_test_lock();
    assert_eq!(
        crate::diag::async_writer_result(),
        Ok(()),
        "the real writer thread spawns fine in CI; an Err here would fail \
         every rdev hotkey install with SpawnError::WriterStartup"
    );
    assert!(
        crate::diag::async_writer_installed(),
        "async_writer_result must install the writer as a side effect, \
         exactly as ensure_async_writer does"
    );
}

/// Structural companion: the runtime tests above drive the recording
/// half; this one pins that PRODUCTION routes its spawn through it. The
/// un-fixed code was literally `let _ = thread::Builder::new()...`, so
/// re-introducing that swallow is the regression to catch.
#[test]
fn production_writer_spawn_failure_is_not_swallowed() {
    let install = scan_fn_body("src/rust/diag.rs", "pub fn ensure_async_writer() {");
    assert!(
        install.code.contains("record_writer_spawn_outcome"),
        "ensure_async_writer must record the writer thread's spawn outcome \
         so `async_writer_result` can report a dead pipeline. Offending \
         function body:\n{}",
        install.raw
    );
    assert!(
        !install.code.contains("let _ = thread::Builder"),
        "ensure_async_writer must NOT discard the spawn Err - that is the \
         bug: the sender is installed regardless, so every callback-path \
         diagnostic is shed with no line anywhere saying why. Offending \
         function body:\n{}",
        install.raw
    );
}
