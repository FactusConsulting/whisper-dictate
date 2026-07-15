use super::test_support::{EnvVarGuard, ENV_LOCK};
use super::*;

#[test]
fn app_root_can_be_inferred_from_installed_exe_directory() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join(if cfg!(windows) {
        "whisper-dictate.exe"
    } else {
        "whisper-dictate"
    });
    let runtime = dir
        .path()
        .join("src")
        .join("python")
        .join("whisper_dictate")
        .join("runtime.py");
    std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
    std::fs::write(runtime, "").unwrap();

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
