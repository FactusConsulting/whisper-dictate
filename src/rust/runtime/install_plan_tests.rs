use super::test_support::{EnvVarGuard, ENV_LOCK};
use super::*;

#[test]
fn install_plan_uses_named_cpu_requirements_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("requirements")).unwrap();
    std::fs::write(dir.path().join("requirements").join("cpu.txt"), "").unwrap();
    let plan = InstallPlan::from_parts(
        dir.path().to_path_buf(),
        requirements_path(dir.path()).unwrap(),
        PathBuf::from("/venv/bin/python"),
        None,
    );

    assert_eq!(
        plan.requirements,
        dir.path().join("requirements").join("cpu.txt")
    );
    assert_eq!(
        plan.install_commands[1].args,
        vec![
            "-m",
            "pip",
            "install",
            "-r",
            plan.requirements.to_str().unwrap()
        ]
    );
}

#[test]
fn install_plan_accepts_legacy_requirements_file_from_old_linux_bundles() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("requirements.txt"), "").unwrap();

    assert_eq!(
        requirements_path(dir.path()).unwrap(),
        dir.path().join("requirements.txt")
    );
}

#[test]
fn install_plan_accepts_legacy_named_requirements_file_from_old_bundles() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("requirements-cpu.txt"), "").unwrap();

    assert_eq!(
        requirements_path(dir.path()).unwrap(),
        dir.path().join("requirements-cpu.txt")
    );
}

#[test]
fn install_plan_skips_legacy_parakeet_requirements_after_backend_removal() {
    // Wave 8 of #348: the Parakeet backend (and `requirements/parakeet.txt`)
    // were removed. Even if a stale file is sitting next to the venv from
    // an older checkout, the installer must no longer pick it up — the
    // optional-requirements hook only handles `requirements/gpu.txt` now.
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("requirements")).unwrap();
    std::fs::write(dir.path().join("requirements").join("cpu.txt"), "").unwrap();
    std::fs::write(dir.path().join("requirements").join("parakeet.txt"), "").unwrap();

    let _python_guard = EnvVarGuard::set(PYTHON_ENV, "/custom/python");
    // The legacy env-var opt-in is gone; even setting it must NOT add an
    // install command for parakeet requirements any more.
    let _backend_guard = EnvVarGuard::set("VOICEPI_STT_BACKEND", "parakeet");
    let plan = InstallPlan::for_current_environment(dir.path().to_path_buf()).unwrap();

    // Base + pip-upgrade only — no third command for the legacy parakeet bundle.
    assert_eq!(plan.install_commands.len(), 2);
    let pip_install = plan.install_commands[1].args.last().unwrap();
    assert!(!pip_install.contains("parakeet"));
}

#[test]
fn install_plan_includes_gpu_requirements_when_cuda_device_requests_it() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("requirements")).unwrap();
    std::fs::write(dir.path().join("requirements").join("cpu.txt"), "").unwrap();
    std::fs::write(dir.path().join("requirements").join("gpu.txt"), "").unwrap();

    let _python_guard = EnvVarGuard::set(PYTHON_ENV, "/custom/python");
    let _device_guard = EnvVarGuard::set("VOICEPI_DEVICE", "cuda");
    let plan = InstallPlan::for_current_environment(dir.path().to_path_buf()).unwrap();

    assert_eq!(plan.install_commands.len(), 3);
    assert_eq!(
        plan.install_commands[2].args[4],
        dir.path()
            .join("requirements")
            .join("gpu.txt")
            .display()
            .to_string()
    );
}

#[test]
fn venv_paths_match_platform_conventions() {
    let home = PathBuf::from("/home/person");
    assert_eq!(
        venv_python_path(&default_venv_dir(&home, Platform::Unix), Platform::Unix),
        PathBuf::from("/home/person/.venv-whisper-dictate/bin/python")
    );

    // Fresh install: Windows resolution consults the real filesystem, so use a
    // guaranteed-empty tempdir as home (a hard-coded C:\Users\... could flake
    // on a machine where such a venv directory actually exists).
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    assert_eq!(
        venv_python_path(
            &default_venv_dir(&home, Platform::Windows),
            Platform::Windows
        ),
        home.join("whisper-dictate-venv")
            .join("Scripts")
            .join("python.exe")
    );
}

#[test]
fn windows_venv_dir_prefers_new_name_when_it_exists() {
    let dir = tempfile::tempdir().unwrap();
    let new_venv = dir.path().join("whisper-dictate-venv");
    std::fs::create_dir_all(&new_venv).unwrap();
    // Also create the legacy dir to confirm new takes priority.
    let legacy_venv = dir.path().join("voice-pi-venv");
    std::fs::create_dir_all(&legacy_venv).unwrap();

    assert_eq!(windows_venv_dir(dir.path()), new_venv);
}

#[test]
fn windows_venv_dir_falls_back_to_legacy_when_only_legacy_exists() {
    let dir = tempfile::tempdir().unwrap();
    let legacy_venv = dir.path().join("voice-pi-venv");
    std::fs::create_dir_all(&legacy_venv).unwrap();

    assert_eq!(windows_venv_dir(dir.path()), legacy_venv);
}

#[test]
fn windows_venv_dir_returns_new_name_for_fresh_install() {
    let dir = tempfile::tempdir().unwrap();
    // Neither directory exists.
    assert_eq!(
        windows_venv_dir(dir.path()),
        dir.path().join("whisper-dictate-venv")
    );
}
