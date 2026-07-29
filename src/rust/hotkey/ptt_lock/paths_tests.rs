//! Companion tests for `ptt_lock/paths.rs`.
//!
//! The resolver is pure (every candidate is an argument), so these run
//! without touching process env and cannot race the rest of the test
//! binary.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::paths::{
    lock_path_in, owner_path_in, resolve_lock_dir, resolve_name_suffix, resolve_user_tag, LockDir,
    Platform,
};

fn os(value: &str) -> Option<OsString> {
    Some(OsString::from(value))
}

fn per_user(dir: &str) -> LockDir {
    LockDir {
        dir: PathBuf::from(dir),
        per_user: true,
    }
}

#[test]
fn the_explicit_override_wins_over_everything() {
    for platform in [Platform::Unix, Platform::Windows] {
        let resolved = resolve_lock_dir(
            platform,
            os("/override"),
            os("/run/user/1000"),
            os("C:\\Users\\a\\AppData\\Local"),
            PathBuf::from("/tmp"),
        );
        assert_eq!(resolved, per_user("/override"), "platform {platform:?}");
    }
}

#[test]
fn an_override_is_treated_as_per_user_so_the_file_name_stays_fixed() {
    // The override IS the isolation mechanism (tests, and operators who
    // deliberately want two independent instances). Two processes pointed
    // at the same override directory must always meet on one file name,
    // regardless of how their shells set USER / USERNAME.
    let resolved = resolve_lock_dir(
        Platform::Windows,
        os("C:\\scratch\\lockdir"),
        None,
        None,
        PathBuf::from("C:\\Temp"),
    );
    assert!(resolved.per_user);
    assert_eq!(
        resolve_name_suffix(resolved.per_user, Some("bob".into())),
        ""
    );
}

#[test]
fn xdg_runtime_dir_is_preferred_on_unix_when_there_is_no_override() {
    // Per-user tmpfs: the right home for a per-user lock on Linux, and it
    // is cleared on logout so it cannot accumulate files.
    let resolved = resolve_lock_dir(
        Platform::Unix,
        None,
        os("/run/user/1000"),
        None,
        PathBuf::from("/tmp"),
    );
    assert_eq!(resolved, per_user("/run/user/1000"));
}

#[test]
fn windows_ignores_xdg_runtime_dir_entirely() {
    // Codex P2 #688. Git Bash / MSYS2 / WSL-adjacent shells export
    // XDG_RUNTIME_DIR on Windows. If it won there, a tray GUI started
    // from Explorer and a CLI started from such a shell would resolve
    // DIFFERENT lock files -- both acquisitions succeed and the guard
    // protects nothing, in precisely the two-process case it exists for.
    let resolved = resolve_lock_dir(
        Platform::Windows,
        None,
        os("/c/Users/a/.xdg-runtime"),
        os("C:\\Users\\a\\AppData\\Local"),
        PathBuf::from("C:\\Temp"),
    );
    assert_eq!(
        resolved.dir,
        Path::new("C:\\Users\\a\\AppData\\Local").join("WhisperDictate"),
        "LOCALAPPDATA is the one canonical per-user location on Windows"
    );

    // ... and with no LOCALAPPDATA it must fall to the temp dir rather
    // than pick up the POSIX-looking path.
    let fallback = resolve_lock_dir(
        Platform::Windows,
        None,
        os("/c/Users/a/.xdg-runtime"),
        None,
        PathBuf::from("C:\\Temp"),
    );
    assert_eq!(fallback.dir, PathBuf::from("C:\\Temp"));
}

#[test]
fn local_app_data_gets_the_whisper_dictate_subdirectory() {
    // Same home as the GUI diagnostic log, so a support thread finds both
    // artefacts in one place.
    let resolved = resolve_lock_dir(
        Platform::Windows,
        None,
        None,
        os("C:\\Users\\a\\AppData\\Local"),
        PathBuf::from("C:\\Temp"),
    );
    assert_eq!(
        resolved.dir,
        Path::new("C:\\Users\\a\\AppData\\Local").join("WhisperDictate")
    );
    assert!(resolved.per_user);
}

#[test]
fn unix_ignores_local_app_data() {
    // The mirror of the Windows gate: a Wine / cross-compile environment
    // that exports LOCALAPPDATA must not pull the lock out of the
    // per-user runtime directory convention.
    let resolved = resolve_lock_dir(
        Platform::Unix,
        None,
        None,
        os("C:\\Users\\a\\AppData\\Local"),
        PathBuf::from("/tmp"),
    );
    assert_eq!(resolved.dir, PathBuf::from("/tmp"));
}

#[test]
fn only_the_unix_temp_fallback_is_treated_as_shared() {
    // This is what decides whether the file name carries an account tag.
    // `/tmp` is shared by every account; every other candidate, on either
    // platform, already separates users by directory.
    assert!(
        !resolve_lock_dir(Platform::Unix, None, None, None, PathBuf::from("/tmp")).per_user,
        "/tmp is shared, so the file name must carry the account"
    );
    assert!(
        resolve_lock_dir(
            Platform::Windows,
            None,
            None,
            None,
            PathBuf::from("C:\\Temp")
        )
        .per_user,
        "Windows temp_dir() is %LOCALAPPDATA%\\Temp - already per-user, and tagging \
         it would reintroduce the USER / USERNAME split"
    );
}

#[test]
fn temp_dir_is_the_last_resort() {
    for platform in [Platform::Unix, Platform::Windows] {
        let resolved = resolve_lock_dir(platform, None, None, None, PathBuf::from("/tmp"));
        assert_eq!(resolved.dir, PathBuf::from("/tmp"), "platform {platform:?}");
    }
}

#[test]
fn the_current_platform_matches_the_build_target() {
    let expected = if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Unix
    };
    assert_eq!(Platform::current(), expected);
}

#[test]
fn blank_candidates_are_treated_as_unset() {
    // `XDG_RUNTIME_DIR=` in a login script is a leftover, not a choice.
    // Resolving to the empty path would put the lock in the process CWD,
    // which on an installed Windows layout is `C:\Program Files\`.
    let unix = resolve_lock_dir(
        Platform::Unix,
        os("  "),
        os(""),
        None,
        PathBuf::from("/tmp"),
    );
    assert_eq!(unix.dir, PathBuf::from("/tmp"));
    let win = resolve_lock_dir(
        Platform::Windows,
        os("  "),
        None,
        os(" "),
        PathBuf::from("C:\\Temp"),
    );
    assert_eq!(win.dir, PathBuf::from("C:\\Temp"));
}

// ---------------------------------------------------------------------
// File-name suffix. Codex P2 #688: tagging a per-user directory is not
// merely redundant, it is actively harmful on Windows.
// ---------------------------------------------------------------------

#[test]
fn a_per_user_directory_never_carries_an_account_tag() {
    // `USER` and `USERNAME` can disagree for the SAME Windows account --
    // Explorer sets only `USERNAME`, Git Bash / WSL export a `USER` that
    // may differ. Tagging inside `%LOCALAPPDATA%` would then give one
    // account two file names in one directory: both acquisitions succeed
    // and the guard protects nothing.
    assert_eq!(resolve_name_suffix(true, Some("Lars".into())), "");
    assert_eq!(resolve_name_suffix(true, Some("CORP\\Lars".into())), "");
    assert_eq!(resolve_name_suffix(true, None), "");
}

#[test]
fn a_shared_directory_separates_accounts_by_name() {
    // The mirror requirement: on a shared `/tmp`, one user's running GUI
    // must not refuse a DIFFERENT user's launch.
    assert_eq!(resolve_name_suffix(false, Some("alice".into())), "-alice");
    assert_ne!(
        resolve_name_suffix(false, Some("alice".into())),
        resolve_name_suffix(false, Some("bob".into()))
    );
    assert_eq!(resolve_name_suffix(false, None), "-unknown");
}

#[test]
fn the_shared_directory_tag_is_a_safe_file_name_component() {
    // Real account names contain spaces, backslashes (DOMAIN\user) and
    // non-ASCII. None of those may reach a path component verbatim.
    assert_eq!(
        resolve_name_suffix(false, Some("CORP\\Lars W".into())),
        "-CORP_Lars_W"
    );
    assert_eq!(resolve_user_tag(Some("  ".into())), "unknown");
    assert!(resolve_name_suffix(false, Some("Lars \u{00c5}".into())).is_ascii());
}

#[test]
fn the_lock_and_owner_files_are_siblings_that_differ_only_by_extension() {
    // The owner record must NOT live inside the lock file: on Windows the
    // locked byte range is unreadable to the very process that needs the
    // holder's pid.
    let dir = Path::new("/run/user/1000");
    let lock = lock_path_in(dir, "");
    let owner = owner_path_in(dir, "");
    assert_ne!(lock, owner);
    assert_eq!(lock.parent(), owner.parent());
    assert_eq!(lock.extension().and_then(|e| e.to_str()), Some("lock"));
    assert_eq!(owner.extension().and_then(|e| e.to_str()), Some("owner"));
    assert_eq!(lock.file_stem(), owner.file_stem());
    assert_eq!(
        lock.file_name().and_then(|n| n.to_str()),
        Some("whisper-dictate-ptt.lock"),
        "a per-user directory uses the bare name"
    );
    assert_eq!(
        lock_path_in(Path::new("/tmp"), "-alice")
            .file_name()
            .and_then(|n| n.to_str()),
        Some("whisper-dictate-ptt-alice.lock"),
        "a shared directory carries the account"
    );
}
