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
