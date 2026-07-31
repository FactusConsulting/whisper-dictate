use super::test_support::{EnvVarGuard, ENV_LOCK};
use super::*;

#[test]
fn app_root_can_be_inferred_from_installed_native_resources() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("whisper-dictate");
    let corpus = dir.path().join("benchmark").join("corpus.json");
    std::fs::create_dir_all(corpus.parent().unwrap()).unwrap();
    std::fs::write(corpus, r#"{"items":[]}"#).unwrap();

    assert_eq!(app_root_from_exe_path(&exe), Some(dir.path().to_path_buf()));
}

#[test]
fn version_prefers_version_file_without_v_prefix() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("VERSION"), "v9.8.7\n").unwrap();
    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, dir.path());

    assert_eq!(version(), "9.8.7");
}
