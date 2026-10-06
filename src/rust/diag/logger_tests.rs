//! logger diagnostic regressions.

use super::{diag_test_lock, scan_fn_body, FailingWriter};
use crate::diag::{init_from_env, install_gui_diagnostic_log, reset_level_for_tests, LOG_ENV_VAR};

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

/// Exercise the failing-stderr path with a writer that returns an error.
///
/// The scanner below cannot catch `writeln!(handle, ...).unwrap()` or
/// `.expect()`, which would restore the exact panic this contract
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
         contract exists to prevent. \
         Tee contents: {contents:?}"
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
    // before the failing-sink test runs. `write_line_to` is the sink
    // half that owns both writes (see `diag.rs`).
    let body = scan_fn_body(
        "src/rust/diag/logger.rs",
        "pub(crate) fn write_line_to<W: Write>(mut stderr_sink: W, line: &str) {",
    );
    assert!(
        !body.code.contains("eprintln!"),
        "write_line_to MUST NOT use `eprintln!` — it panics on stderr \
         write failure and closes the GUI diagnostic path on Windows. \
         Offending function body:\n{}",
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
             discarded via `let _ =`. \
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
    // Release the stderr guard
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
// The self-test warning must use the fallible diagnostic writer, not
// `eprintln!`, so a closed stderr cannot abort the launcher.
// -----------------------------------------------------------------------

#[test]
fn hotkey_boot_self_test_dispatcher_does_not_use_eprintln_for_config_warning() {
    let body = scan_fn_body(
        "src/rust/main_self_test.rs",
        "fn handle_self_test_hotkey_boot(",
    );
    assert!(
        !body.code.contains("eprintln!"),
        "handle_self_test_hotkey_boot MUST NOT use `eprintln!` — it panics \
         on stderr write failure and would abort the CLI before \
         `run_boot_test` runs on a closed / redirected stderr (the exact \
         Windows/hidden-launcher failure `diag::write_line` was rewritten \
         to survive). Route the warning through \
         `whisper_dictate_app::diag::write_line(...)` instead. \
         Offending function body:\n{}",
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
         run."
    );
}

// -----------------------------------------------------------------------
// The callback-thread chord trace must use asynchronous diagnostic logging;
// synchronous writes can exceed the Windows hook time budget.
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
         Offending function body:\n{}",
        body.raw
    );
    // Sanity: the fix's async path must still be present — a
    // regression that dropped the trace entirely would remove the
    // `[chord]` line that the redaction tests below rely on.
    assert!(
        body.raw.contains("log_async!"),
        "KeyTracker::handle must still emit the `[chord]` trace at \
         debug level (via `crate::diag::log_async!`). Regression \
         that dropped the trace would silence the wedge diagnostic."
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
