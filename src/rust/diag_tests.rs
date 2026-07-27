//! Companion tests for [`crate::diag`]. Extracted from inline
//! `#[cfg(test)] mod tests` in `diag.rs` so the regression-test
//! discipline scanner (per AGENTS.md, `enforce-regression-test-discipline`)
//! sees a matching test file next to the production module.

#![cfg(test)]

use crate::diag::{default_gui_diagnostic_path, install_gui_diagnostic_log};
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
