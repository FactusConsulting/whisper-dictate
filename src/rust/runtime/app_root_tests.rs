use super::test_support::EnvVarGuard;
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
    let _guard = crate::test_env_lock::lock_env();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("VERSION"), "v9.8.7\n").unwrap();
    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, dir.path());

    assert_eq!(version(), "9.8.7");
}

#[test]
fn version_falls_back_to_cargo_version_without_spawning_git() {
    let _guard = crate::test_env_lock::lock_env();
    let dir = tempfile::tempdir().unwrap();
    // Make the app root a git checkout with a describable commit so a
    // `git describe` fallback would produce a hash, not the crate version.
    // Drop any ambient worktree pointers (devcontainer runs force GIT_DIR /
    // GIT_WORK_TREE through cargo's [env]) so the commands act on the tempdir.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    };
    if !git(&["init", "-q"]) {
        eprintln!("git unavailable; skipping");
        return;
    }
    std::fs::write(dir.path().join("README"), "x").unwrap();
    assert!(git(&["add", "README"]));
    assert!(git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "init"
    ]));
    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, dir.path());

    assert_eq!(version(), env!("CARGO_PKG_VERSION"));
}
