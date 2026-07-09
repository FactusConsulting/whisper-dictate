use super::test_support::{EnvVarGuard, ENV_LOCK};
use super::*;

#[test]
fn app_root_prefers_env_override() {
    // The `VOICEPI_APP_ROOT` override wins over the exe-based
    // resolution -- packaging tests + the dev checkout both rely on
    // it. Kept post-Wave-8: the exe fallback now simply returns the
    // exe's parent (no marker required), so the override remains the
    // only way to force a different root.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, dir.path());
    assert_eq!(app_root(), dir.path().to_path_buf());
}

#[test]
fn version_prefers_version_file_without_v_prefix() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("VERSION"), "v9.8.7\n").unwrap();
    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, dir.path());

    assert_eq!(version(), "9.8.7");
}

/// Codex #453 P2: `app_root_from_exe_path` must only accept the exe's
/// parent when a native-bundle marker (`VERSION` file OR the
/// packaging setup.sh) is present. Otherwise a `cargo run` /
/// `cargo test` binary sitting under `target/debug/` would return
/// that directory and the documented fallback to `source_root()` for
/// shipped resources (`benchmark/corpus.json` etc.) would never fire.
#[test]
fn app_root_from_exe_path_accepts_version_marker() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("VERSION"), "1.20.0\n").unwrap();
    let fake_exe = dir.path().join("whisper-dictate");
    assert_eq!(
        app_root_from_exe_path(&fake_exe),
        Some(dir.path().to_path_buf())
    );
}

#[test]
fn app_root_from_exe_path_accepts_packaging_marker() {
    let dir = tempfile::tempdir().unwrap();
    let packaging = dir
        .path()
        .join("packaging")
        .join("linux")
        .join("ubuntu26.04");
    std::fs::create_dir_all(&packaging).unwrap();
    std::fs::write(packaging.join("setup.sh"), "#!/bin/sh\n").unwrap();
    let fake_exe = dir.path().join("whisper-dictate");
    assert_eq!(
        app_root_from_exe_path(&fake_exe),
        Some(dir.path().to_path_buf())
    );
}

#[test]
fn app_root_from_exe_path_rejects_bare_target_directory() {
    // No VERSION file, no packaging/linux/ubuntu26.04/setup.sh -- a
    // stray binary in an arbitrary directory (or the classic
    // `target/debug/` under a checkout) must be rejected so `app_root`
    // falls through to `source_root()`.
    let dir = tempfile::tempdir().unwrap();
    let fake_exe = dir.path().join("whisper-dictate");
    assert!(app_root_from_exe_path(&fake_exe).is_none());
}
