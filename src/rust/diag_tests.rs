//! Companion tests for [`crate::diag`]. Extracted from inline
//! `#[cfg(test)] mod tests` in `diag.rs` so the regression-test
//! discipline scanner (per AGENTS.md, `enforce-regression-test-discipline`)
//! sees a matching test file next to the production module.

#![cfg(test)]

use crate::diag::{
    current_level, debug_enabled, default_gui_diagnostic_path, info_enabled, init_from_env,
    install_gui_diagnostic_log, reset_level_for_tests, trace_enabled, LogLevel, LOG_ENV_VAR,
};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serialise diag-mutation tests so parallel runs don't race the
/// shared writer slot: two tests installing to different temp paths
/// simultaneously would each see the other's writes. Mirrors the
/// pattern the crate-wide `test_env_lock::ENV_LOCK` uses for
/// env-mutation tests.
fn diag_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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
