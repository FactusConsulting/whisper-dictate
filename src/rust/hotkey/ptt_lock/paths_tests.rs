//! Companion tests for `ptt_lock/paths.rs`.
//!
//! The resolver is pure (every candidate is an argument), so these run
//! without touching process env and cannot race the rest of the test
//! binary.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::paths::{lock_path_in, owner_path_in, resolve_lock_dir, resolve_user_tag};

fn os(value: &str) -> Option<OsString> {
    Some(OsString::from(value))
}

#[test]
fn the_explicit_override_wins_over_everything() {
    let dir = resolve_lock_dir(
        os("/override"),
        os("/run/user/1000"),
        os("C:\\Users\\a\\AppData\\Local"),
        PathBuf::from("/tmp"),
    );
    assert_eq!(dir, PathBuf::from("/override"));
}

#[test]
fn xdg_runtime_dir_is_preferred_when_there_is_no_override() {
    // Per-user tmpfs: the right home for a per-user lock on Linux, and it
    // is cleared on logout so it cannot accumulate files.
    let dir = resolve_lock_dir(None, os("/run/user/1000"), None, PathBuf::from("/tmp"));
    assert_eq!(dir, PathBuf::from("/run/user/1000"));
}

#[test]
fn local_app_data_gets_the_whisper_dictate_subdirectory() {
    // Same home as the GUI diagnostic log, so a support thread finds both
    // artefacts in one place.
    let dir = resolve_lock_dir(
        None,
        None,
        os("C:\\Users\\a\\AppData\\Local"),
        PathBuf::from("C:\\Temp"),
    );
    assert_eq!(
        dir,
        Path::new("C:\\Users\\a\\AppData\\Local").join("WhisperDictate")
    );
}

#[test]
fn temp_dir_is_the_last_resort() {
    let dir = resolve_lock_dir(None, None, None, PathBuf::from("/tmp"));
    assert_eq!(dir, PathBuf::from("/tmp"));
}

#[test]
fn blank_candidates_are_treated_as_unset() {
    // `XDG_RUNTIME_DIR=` in a login script is a leftover, not a choice.
    // Resolving to the empty path would put the lock in the process CWD,
    // which on an installed Windows layout is `C:\Program Files\`.
    let dir = resolve_lock_dir(os("  "), os(""), None, PathBuf::from("/tmp"));
    assert_eq!(dir, PathBuf::from("/tmp"));
}

#[test]
fn the_user_tag_keeps_the_lock_per_user_in_a_shared_temp_dir() {
    // On Linux `/tmp` is shared by every account. Without the tag, one
    // user's running GUI would refuse a different user's launch -- a
    // false refusal, the one failure mode this guard must not introduce.
    assert_ne!(
        resolve_user_tag(Some("alice".into())),
        resolve_user_tag(Some("bob".into()))
    );
    assert_eq!(resolve_user_tag(Some("alice".into())), "alice");
}

#[test]
fn the_user_tag_is_sanitised_into_a_safe_file_name_component() {
    // Real account names contain spaces, backslashes (DOMAIN\user) and
    // non-ASCII. None of those may reach a path component verbatim.
    assert_eq!(resolve_user_tag(Some("CORP\\Lars W".into())), "CORP_Lars_W");
    assert_eq!(resolve_user_tag(Some("  ".into())), "unknown");
    assert_eq!(resolve_user_tag(None), "unknown");
    assert!(resolve_user_tag(Some("Lars \u{00c5}".into())).is_ascii());
}

#[test]
fn the_lock_and_owner_files_are_siblings_that_differ_only_by_extension() {
    // The owner record must NOT live inside the lock file: on Windows the
    // locked byte range is unreadable to the very process that needs the
    // holder's pid.
    let dir = Path::new("/run/user/1000");
    let lock = lock_path_in(dir, "alice");
    let owner = owner_path_in(dir, "alice");
    assert_ne!(lock, owner);
    assert_eq!(lock.parent(), owner.parent());
    assert_eq!(lock.extension().and_then(|e| e.to_str()), Some("lock"));
    assert_eq!(owner.extension().and_then(|e| e.to_str()), Some("owner"));
    assert_eq!(lock.file_stem(), owner.file_stem());
    assert_eq!(
        lock.file_name().and_then(|n| n.to_str()),
        Some("whisper-dictate-ptt-alice.lock")
    );
}
