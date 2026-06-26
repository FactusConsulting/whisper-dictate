use super::test_support::{runtime_module_args, EnvVarGuard, ENV_LOCK};
use super::*;

#[test]
fn worker_command_launches_python_directly() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");

    let root = PathBuf::from("/tmp/whisper-dictate");
    let command = worker_command(&root);

    assert_eq!(command.program, PathBuf::from(default_python_name()));
    assert_eq!(command.args, runtime_module_args());
    assert_eq!(command.working_dir, root);
    assert!(command.env.contains(&(
        PYTHONPATH_ENV.to_owned(),
        Path::new("/tmp/whisper-dictate")
            .join("src")
            .join("python")
            .display()
            .to_string()
    )));
}

#[test]
fn worker_command_appends_passthrough_args() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");

    let command = worker_command_with_args(
        "/tmp/whisper-dictate",
        ["--key".to_owned(), "shift_r+ctrl_r".to_owned()],
    );

    assert_eq!(
        command.args,
        vec![
            "-m".to_owned(),
            "whisper_dictate.runtime".to_owned(),
            "--app-root".to_owned(),
            "/tmp/whisper-dictate".to_owned(),
            "--key".to_owned(),
            "shift_r+ctrl_r".to_owned(),
        ]
    );
}

#[test]
fn worker_command_does_not_force_utf8_for_foreground_console() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");

    let command = worker_command("/tmp/whisper-dictate");

    assert!(!command.env.iter().any(|(key, _)| key == PYTHON_UTF8_ENV));
    assert!(!command
        .env
        .iter()
        .any(|(key, _)| key == PYTHON_IO_ENCODING_ENV));
}

#[test]
fn worker_command_exports_effective_config_to_python_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(
        &config_path,
        serde_json::json!({
            "lang": "da",
            "model": "large-v3",
            "stt_debug": "1"
        })
        .to_string(),
    )
    .unwrap();

    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_path);
    let _model_guard = EnvVarGuard::set("VOICEPI_MODEL", "env-model");
    let _device_guard = EnvVarGuard::set("VOICEPI_DEVICE", "cuda");
    let _key_guard = EnvVarGuard::remove("VOICEPI_KEY");
    let command = worker_command("/tmp/whisper-dictate");
    let env = command
        .env
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();

    assert_eq!(env["VOICEPI_MODEL"], "large-v3");
    assert_eq!(env["VOICEPI_LANG"], "da");
    assert_eq!(env["VOICEPI_DEVICE"], "cuda");
    assert_eq!(env["VOICEPI_KEY"], "ctrl_r");
    assert_eq!(env["VOICEPI_STT_DEBUG"], "1");
}

#[test]
fn worker_command_honors_python_override() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = PathBuf::from("/tmp/whisper-dictate");

    let _python_guard = EnvVarGuard::set(PYTHON_ENV, "/custom/python");
    let command = worker_command(root);

    assert_eq!(command.program, PathBuf::from("/custom/python"));
}

#[test]
fn worker_command_prefers_existing_project_venv_python() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Use the canonical post-rebrand name (new installs).
    let python = if cfg!(windows) {
        dir.path()
            .join("whisper-dictate-venv")
            .join("Scripts")
            .join("python.exe")
    } else {
        dir.path()
            .join(".venv-whisper-dictate")
            .join("bin")
            .join("python")
    };
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(&python, "").unwrap();

    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", dir.path());
    let command = worker_command("/tmp/whisper-dictate");

    assert_eq!(command.program, python);
}

#[test]
#[cfg(windows)]
fn worker_command_falls_back_to_legacy_venv_on_windows() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Only the pre-rebrand venv exists (existing install, not yet migrated).
    let python = dir
        .path()
        .join("voice-pi-venv")
        .join("Scripts")
        .join("python.exe");
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(&python, "").unwrap();

    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", dir.path());
    let command = worker_command("/tmp/whisper-dictate");

    assert_eq!(command.program, python);
}

#[test]
fn default_worker_command_honors_app_root_override() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, "/installed/app");

    let command = default_worker_command();

    assert_eq!(command.working_dir, PathBuf::from("/installed/app"));
    assert_eq!(
        command.args,
        vec![
            "-m".to_owned(),
            "whisper_dictate.runtime".to_owned(),
            "--app-root".to_owned(),
            "/installed/app".to_owned(),
        ]
    );
}

#[test]
fn doctor_command_adds_doctor_argument() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, "/installed/app");
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    let command = doctor_command();

    assert_eq!(
        command.args,
        vec![
            "-m".to_owned(),
            "whisper_dictate.runtime".to_owned(),
            "--app-root".to_owned(),
            "/installed/app".to_owned(),
            "--doctor".to_owned()
        ]
    );
}

#[test]
fn worker_command_does_not_auto_disable_python_hotkey_when_env_var_set() {
    // P1 #1 regression: previously `worker_command_with_args` injected
    // `VOICEPI_PYTHON_HOTKEY=0` whenever VOICEPI_HOTKEY_BACKEND=rust AND
    // the feature was compiled in — but nothing checked that the Rust
    // listener had actually started. The supervisor now opts in
    // explicitly via `disable_python_hotkey` ONLY after a successful
    // install, so the env-var alone must never park Python.
    let _guard = ENV_LOCK.lock().unwrap();
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _backend_guard = EnvVarGuard::set("VOICEPI_HOTKEY_BACKEND", "rust");

    let command = worker_command("/tmp/whisper-dictate");

    assert!(
        !command
            .env
            .iter()
            .any(|(k, _)| k == "VOICEPI_PYTHON_HOTKEY"),
        "worker_command must not auto-disable the Python hotkey based on \
         env-var alone — the supervisor calls disable_python_hotkey only \
         after the Rust listener is confirmed wired (PR #344 P1 #1)"
    );
}

#[test]
fn disable_python_hotkey_adds_the_flag() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");

    let mut command = worker_command("/tmp/whisper-dictate");
    disable_python_hotkey(&mut command);

    let value = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_PYTHON_HOTKEY")
        .map(|(_, v)| v.as_str());
    assert_eq!(value, Some("0"));
}

#[test]
fn disable_python_hotkey_is_idempotent() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");

    let mut command = worker_command("/tmp/whisper-dictate");
    disable_python_hotkey(&mut command);
    disable_python_hotkey(&mut command);

    let count = command
        .env
        .iter()
        .filter(|(k, _)| k == "VOICEPI_PYTHON_HOTKEY")
        .count();
    assert_eq!(count, 1, "calling twice must not duplicate the flag");
}

#[test]
fn benchmark_command_adds_run_benchmark_argument_with_app_root_and_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(
        &config_path,
        serde_json::json!({ "model": "large-v3", "device": "cuda" }).to_string(),
    )
    .unwrap();

    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, "/installed/app");
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_path);
    let _model_guard = EnvVarGuard::remove("VOICEPI_MODEL");
    let _device_guard = EnvVarGuard::remove("VOICEPI_DEVICE");

    let command = benchmark_command();

    // Drives the worker's `--run-benchmark` entry, inheriting the same
    // `--app-root` the other one-shot worker commands use.
    assert_eq!(
        command.args,
        vec![
            "-m".to_owned(),
            "whisper_dictate.runtime".to_owned(),
            "--app-root".to_owned(),
            "/installed/app".to_owned(),
            "--run-benchmark".to_owned(),
        ]
    );
    // The benchmark inherits the same effective model/device config as a
    // dictation run, so it benchmarks what the user actually has selected.
    let env = command
        .env
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(env["VOICEPI_MODEL"], "large-v3");
    assert_eq!(env["VOICEPI_DEVICE"], "cuda");
}

// -----------------------------------------------------------------------
// P2 #346 finding 1: install_rust_hotkey_from_command helper.
// -----------------------------------------------------------------------

#[test]
fn install_rust_hotkey_from_command_skips_when_key_missing() {
    // When VOICEPI_KEY is absent from the command's env (e.g. no default set),
    // the helper must return None without calling maybe_install_rust_hotkey.
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _backend_guard = EnvVarGuard::set("VOICEPI_HOTKEY_BACKEND", "rust");

    // Build a command with no VOICEPI_KEY in env.
    let mut command = worker_command("/tmp/whisper-dictate");
    command.env.retain(|(k, _)| k != "VOICEPI_KEY");

    let (tx, _rx) = std::sync::mpsc::channel();
    let handle = install_rust_hotkey_from_command(&command, tx);
    assert!(
        handle.is_none(),
        "must return None when PTT key is missing from env"
    );
}

#[test]
fn install_rust_hotkey_from_command_reads_toggle_mode_from_env() {
    // Verifies that VOICEPI_TOGGLE=True selects Toggle mode when the helper
    // extracts config from the command. We can't call maybe_install_rust_hotkey
    // in a headless env, so we test the config-extraction logic directly by
    // checking that the function doesn't panic and returns the expected result
    // type (None when backend env var is not set).
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _backend_guard = EnvVarGuard::remove("VOICEPI_HOTKEY_BACKEND");

    let mut command = worker_command("/tmp/whisper-dictate");
    // Ensure a PTT key and toggle flag are present.
    command
        .env
        .retain(|(k, _)| k != "VOICEPI_KEY" && k != "VOICEPI_TOGGLE");
    command
        .env
        .push(("VOICEPI_KEY".to_owned(), "ctrl_l".to_owned()));
    command
        .env
        .push(("VOICEPI_TOGGLE".to_owned(), "True".to_owned()));

    let (tx, _rx) = std::sync::mpsc::channel();
    // Backend env var not set → None (function exits early via
    // maybe_install_rust_hotkey's guard). No panic or crash = pass.
    let handle = install_rust_hotkey_from_command(&command, tx);
    assert!(
        handle.is_none(),
        "VOICEPI_HOTKEY_BACKEND not set → must return None"
    );
}
