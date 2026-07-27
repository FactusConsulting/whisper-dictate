//! Tests for [`super::path_util::expand_user`].
//!
//! Split out from `path_util.rs` because the regression-test discipline
//! scanner (`test_regression_test_discipline.py`) looks for a sibling
//! `_tests.rs` file (or `tests_*.rs`) alongside any file that adds a
//! `pub` symbol -- inline `#[cfg(test)] mod tests` blocks are not
//! recognised as "the matching test file", by design (AGENTS.md
//! sections 32-58).

use super::path_util::expand_user;
use std::path::PathBuf;

/// Set a fake HOME so the assertion is deterministic on any machine.
/// Serialised through the crate ENV_LOCK because `set_var` /
/// `remove_var` are process-global.
#[test]
fn expands_leading_tilde_from_home_env() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let saved_home = std::env::var_os("HOME");
    let saved_userprofile = std::env::var_os("USERPROFILE");
    std::env::set_var("HOME", home);
    std::env::set_var("USERPROFILE", home);

    assert_eq!(expand_user("~"), home.to_path_buf());
    assert_eq!(expand_user("~/history.jsonl"), home.join("history.jsonl"));
    // Windows-flavoured separator inside a `~\path` -- must also be stripped.
    assert_eq!(expand_user("~\\history.jsonl"), home.join("history.jsonl"));
    // Absolute paths pass through untouched.
    assert_eq!(
        expand_user("/tmp/history.jsonl"),
        PathBuf::from("/tmp/history.jsonl")
    );

    // Restore env so sibling tests are unaffected.
    match saved_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match saved_userprofile {
        Some(v) => std::env::set_var("USERPROFILE", v),
        None => std::env::remove_var("USERPROFILE"),
    }
}

/// Missing `HOME`/`USERPROFILE` falls through to `.` -- documents the
/// last-resort behaviour the sibling `dictionary::store::expand_user`
/// and `corpus::expand_tilde` helpers share, so a future refactor
/// cannot accidentally start returning a bogus absolute path here.
#[test]
fn missing_home_falls_back_to_current_dir() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved_home = std::env::var_os("HOME");
    let saved_userprofile = std::env::var_os("USERPROFILE");
    std::env::remove_var("HOME");
    std::env::remove_var("USERPROFILE");

    assert_eq!(expand_user("~"), PathBuf::from("."));
    assert_eq!(
        expand_user("~/history.jsonl"),
        PathBuf::from(".").join("history.jsonl")
    );

    match saved_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match saved_userprofile {
        Some(v) => std::env::set_var("USERPROFILE", v),
        None => std::env::remove_var("USERPROFILE"),
    }
}
