//! Companion tests for [`crate::diag`]. Extracted from inline
//! `#[cfg(test)] mod tests` in `diag.rs` so the regression-test
//! discipline scanner (per AGENTS.md, `enforce-regression-test-discipline`)
//! sees a matching test file next to the production module.

#![cfg(test)]

use crate::diag::{
    current_level, debug_enabled, default_gui_diagnostic_path, info_enabled, init_from_env,
    install_gui_diagnostic_log, reset_level_for_tests, trace_enabled, LogLevel, LOG_ENV_VAR,
};
use crate::diag_test_lock::DIAG_WRITER_LOCK;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
struct FnBody {
    raw: String,
    code: String,
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
fn scan_fn_body(rel_path: &str, fn_marker: &str) -> FnBody {
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
    let dropped = AtomicU64::new(0);
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(capacity);

    // ---- Phase 1: the queue ACCOUNTS for what it sheds. ----
    let flood_started = std::time::Instant::now();
    for i in 0..(capacity + overflow) {
        crate::diag::enqueue_async_into(&tx, &pending, &dropped, format!("flood #{i}"));
    }
    let flood_elapsed = flood_started.elapsed();
    let shed = dropped.load(Ordering::Relaxed);
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
        dropped_after: dropped.load(Ordering::Relaxed),
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
    let dropped = AtomicU64::new(0);
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
        dropped.fetch_add(SHED, Ordering::Relaxed);

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
    let dropped = AtomicU64::new(0);
    let handshake_failed = std::sync::atomic::AtomicBool::new(false);
    let recorded = std::sync::Mutex::new(Vec::<String>::new());

    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(capacity);
    // Rendezvous pair: `go` is the writer saying "a slot just came free",
    // `done` is the producer saying "I have refilled it and shed the
    // rest". Unbounded channels so neither send can block.
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    for i in 0..capacity {
        crate::diag::enqueue_async_into(&tx, &pending, &dropped, format!("seed #{i}"));
    }

    let producer_tx = tx.clone();
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
                    pending_ref,
                    dropped_ref,
                    format!("burst #{round}"),
                );
                // ...and everything after it arrives against a full queue.
                for i in 0..shed_per_round {
                    crate::diag::enqueue_async_into(
                        &producer_tx,
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
    let dropped = AtomicU64::new(0);
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(CAPACITY);

    let mut queued: Vec<(u64, String)> = vec![
        (0, "before".to_owned()),
        (3, "gap opens".to_owned()),
        (4, "still shedding".to_owned()),
    ];
    queued.extend((0..clear_run).map(|i| (0, format!("recovered #{i}"))));
    queued.push((2, "second burst".to_owned()));
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
        dropped.load(Ordering::Relaxed),
        0,
        "closing an episode must reset the shared counter"
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
    let dropped = AtomicU64::new(0);
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
    // chance to report it.
    dropped.store(3, Ordering::Relaxed);

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
        dropped.load(Ordering::Relaxed),
        0,
        "closing the episode on the drain must reset the shared counter"
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

    let sender = scan_fn_body("src/rust/diag.rs", "pub(crate) fn enqueue_async_into(");
    assert!(
        sender.code.contains("TrySendError::Full"),
        "enqueue_async_into must match on `TrySendError::Full` — an \
         `is_ok()` shortcut collapses `Full` and `Disconnected` into one \
         silent discard and loses the drop accounting. Offending \
         function body:\n{}",
        sender.raw
    );
    assert!(
        sender.code.contains("dropped.fetch_add"),
        "enqueue_async_into must bump the drop counter on a full queue, \
         otherwise the writer has nothing to report. Offending function \
         body:\n{}",
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
        sender.code.contains("take_pending_drops") && sender.code.contains("drops_before"),
        "enqueue_async_into must bind the outstanding shed count to the \
         record it is accepting (an `AsyncRecord::Line` carrying \
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
    let dropped = AtomicU64::new(0);
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
            crate::diag::enqueue_async_into(&tx, &pending, &dropped, format!("record #{i}"));
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
        let acked = crate::diag::drain_and_shutdown_into(&tx, std::time::Duration::from_secs(10));
        let elapsed = started.elapsed();
        // A second drain against a writer that already stopped must
        // return PROMPTLY rather than sit out the deadline: teardown can
        // run twice on a nested error path. Whether it reports success
        // (`Disconnected` on the way in) or failure (the ack sender died
        // with the receiver) is a race with the writer thread's final
        // drop, and either answer is honest - the stall is the bug.
        let second_started = std::time::Instant::now();
        let _ = crate::diag::drain_and_shutdown_into(&tx, std::time::Duration::from_secs(10));
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
    let dropped = AtomicU64::new(0);
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
            crate::diag::enqueue_async_into(&tx, &pending, &dropped, format!("wedge #{i}"));
        }

        let started = std::time::Instant::now();
        let acked = crate::diag::drain_and_shutdown_into(&tx, DEADLINE);
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

    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::diag::AsyncRecord>(4);
    // Whatever is still queued dies here, exactly as it would if the
    // writer thread had panicked or never spawned.
    drop(rx);

    let started = std::time::Instant::now();
    let acked = crate::diag::drain_and_shutdown_into(&tx, std::time::Duration::from_millis(500));
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

    let sender = scan_fn_body("src/rust/diag.rs", "pub(crate) fn drain_and_shutdown_into(");
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
        "on the Shutdown sentinel the writer must drain what is still \
         queued (`try_recv`) BEFORE acking; acking first discards exactly \
         the tail of the trace the drain was added to save. Offending \
         function body:\n{}",
        drain_arm.raw
    );
    assert!(
        drain_arm.code.contains("close_burst_with_pending_drops"),
        "the drain must CLOSE any open overload episode before acking \
         (PR #680's `BurstState`): a drain landing mid-burst would \
         otherwise take the process down with the episode's summary \
         marker unwritten, so the tee file's last word on a wedged sink \
         would be the episode's opening count instead of its total. \
         Offending function body:\n{}",
        drain_arm.raw
    );
}
