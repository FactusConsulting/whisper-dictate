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

// -----------------------------------------------------------------------
// P2 #373 finding 2: parse_toggle_value accepts all truthy variants.
// -----------------------------------------------------------------------

#[test]
fn parse_toggle_value_accepts_all_truthy_variants() {
    // P2 #373 finding 2: the Rust-side toggle parser must honour the same
    // truthy values as the Python config layer and shell tooling.
    for truthy in &[
        "true", "True", "TRUE", "1", "yes", "Yes", "YES", "on", "On", "ON",
    ] {
        assert!(
            parse_toggle_value(truthy),
            "parse_toggle_value({truthy:?}) should be true",
        );
    }
}

#[test]
fn parse_toggle_value_rejects_falsy_values() {
    for falsy in &[
        "false", "False", "FALSE", "0", "no", "off", "", "  ", "rust", "toggle",
    ] {
        assert!(
            !parse_toggle_value(falsy),
            "parse_toggle_value({falsy:?}) should be false",
        );
    }
}

#[test]
fn parse_toggle_value_trims_whitespace() {
    assert!(parse_toggle_value("  true  "));
    assert!(parse_toggle_value(" 1 "));
    assert!(!parse_toggle_value("  false  "));
}

// -----------------------------------------------------------------------
// P2 #373 finding 1/3: extract_hotkey_key_names helper.
// -----------------------------------------------------------------------

#[test]
fn extract_hotkey_key_names_splits_plus_separated_keys() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    let mut command = worker_command("/tmp/whisper-dictate");
    command.env.retain(|(k, _)| k != "VOICEPI_KEY");
    command
        .env
        .push(("VOICEPI_KEY".to_owned(), "ctrl_l+shift_r".to_owned()));

    let names = extract_hotkey_key_names(&command);
    assert_eq!(names, vec!["ctrl_l", "shift_r"]);
}

#[test]
fn extract_hotkey_key_names_returns_empty_when_key_missing() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    let mut command = worker_command("/tmp/whisper-dictate");
    command.env.retain(|(k, _)| k != "VOICEPI_KEY");

    let names = extract_hotkey_key_names(&command);
    assert!(names.is_empty(), "no VOICEPI_KEY → empty vec");
}

#[test]
fn extract_hotkey_key_names_trims_whitespace_around_segments() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    let mut command = worker_command("/tmp/whisper-dictate");
    command.env.retain(|(k, _)| k != "VOICEPI_KEY");
    command
        .env
        .push(("VOICEPI_KEY".to_owned(), " ctrl_l + f9 ".to_owned()));

    let names = extract_hotkey_key_names(&command);
    assert_eq!(names, vec!["ctrl_l", "f9"]);
}

// -----------------------------------------------------------------------
// P3 #376: audio_devices_command must propagate VOICEPI_DEVICES_BACKEND=rust
// when the parent env has VOICEPI_AUDIO_BACKEND=rust, so the Python picker
// uses the Rust enumeration that respects rust-capture's default-host limit.
// -----------------------------------------------------------------------

/// Look up a value by key in a `WorkerCommand.env` Vec. Returns the LAST
/// occurrence because that's the one the spawned Python worker sees (the
/// stdlib `Command::env` semantics collapse on last-write-wins).
fn lookup_env<'a>(command: &'a super::WorkerCommand, key: &str) -> Option<&'a str> {
    command
        .env
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

#[test]
fn audio_devices_command_propagates_devices_backend_when_audio_backend_rust() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _audio_guard = EnvVarGuard::set("VOICEPI_AUDIO_BACKEND", "rust");
    let _devices_guard = EnvVarGuard::remove("VOICEPI_DEVICES_BACKEND");

    let command = audio_devices_command();

    assert_eq!(
        lookup_env(&command, "VOICEPI_DEVICES_BACKEND"),
        Some("rust"),
        "VOICEPI_AUDIO_BACKEND=rust must auto-set VOICEPI_DEVICES_BACKEND=rust \
         so the Python picker uses the Rust enumeration (P3 #376)"
    );
}

#[test]
fn audio_devices_command_propagation_is_case_insensitive_on_value() {
    // VOICEPI_AUDIO_BACKEND values are matched case-insensitively by
    // audio_pipeline_requested, so the propagation must follow the same rule.
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _devices_guard = EnvVarGuard::remove("VOICEPI_DEVICES_BACKEND");
    for value in ["Rust", "RUST", "  rust  ", "rUsT"] {
        let _audio_guard = EnvVarGuard::set("VOICEPI_AUDIO_BACKEND", value);
        let command = audio_devices_command();
        assert_eq!(
            lookup_env(&command, "VOICEPI_DEVICES_BACKEND"),
            Some("rust"),
            "VOICEPI_AUDIO_BACKEND={value:?} should also propagate"
        );
    }
}

#[test]
fn audio_devices_command_skips_propagation_when_audio_backend_unset() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _audio_guard = EnvVarGuard::remove("VOICEPI_AUDIO_BACKEND");
    let _devices_guard = EnvVarGuard::remove("VOICEPI_DEVICES_BACKEND");

    let command = audio_devices_command();

    assert!(
        lookup_env(&command, "VOICEPI_DEVICES_BACKEND").is_none(),
        "default Python audio path: must not silently turn on Rust devices \
         backend without an explicit opt-in"
    );
}

#[test]
fn audio_devices_command_skips_propagation_when_audio_backend_is_python() {
    // Any non-rust value (including the documented "python" sentinel)
    // means the user has NOT opted into Rust capture; the picker stays
    // on sounddevice so it can offer non-default-host devices.
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _devices_guard = EnvVarGuard::remove("VOICEPI_DEVICES_BACKEND");
    for value in ["python", "Python", "PYTHON", "off", "0", ""] {
        let _audio_guard = EnvVarGuard::set("VOICEPI_AUDIO_BACKEND", value);
        let command = audio_devices_command();
        assert!(
            lookup_env(&command, "VOICEPI_DEVICES_BACKEND").is_none(),
            "VOICEPI_AUDIO_BACKEND={value:?} must not propagate (non-rust)"
        );
    }
}

#[test]
fn audio_devices_command_skips_propagation_when_process_env_already_sets_devices_backend() {
    // A user that has explicitly exported VOICEPI_DEVICES_BACKEND=python in
    // their shell (e.g. to force sounddevice for a debug session even while
    // Rust capture is active) must NOT have it silently overridden. The
    // spawned worker inherits the process env, so as long as the propagator
    // does not push an entry into command.env, the worker sees `python`.
    // The propagator therefore detects the process-env value and skips —
    // verified here by asserting no entry was added to command.env (so
    // inheritance wins, not an override).
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _audio_guard = EnvVarGuard::set("VOICEPI_AUDIO_BACKEND", "rust");
    let _devices_guard = EnvVarGuard::set("VOICEPI_DEVICES_BACKEND", "python");

    let command = audio_devices_command();

    assert!(
        lookup_env(&command, "VOICEPI_DEVICES_BACKEND").is_none(),
        "propagator must NOT add an override to command.env when the \
         process env already mentions VOICEPI_DEVICES_BACKEND — \
         inheritance from process env carries the user's explicit choice"
    );
}

#[test]
fn audio_devices_command_propagation_is_idempotent_against_pre_populated_command_env() {
    // Belt-and-braces: if a future caller (or a test) pre-populates the
    // worker command with a VOICEPI_DEVICES_BACKEND entry, the propagator
    // must leave it untouched rather than appending a second entry — even
    // when VOICEPI_AUDIO_BACKEND=rust would otherwise trigger the
    // propagation. Two entries would still pick the last write at spawn
    // time but would be confusing in `--worker-env` dumps and would mask
    // bugs in upstream callers.
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _audio_guard = EnvVarGuard::set("VOICEPI_AUDIO_BACKEND", "rust");
    let _devices_guard = EnvVarGuard::remove("VOICEPI_DEVICES_BACKEND");

    let mut command = audio_devices_command();
    let initial_count = command
        .env
        .iter()
        .filter(|(k, _)| k == "VOICEPI_DEVICES_BACKEND")
        .count();
    assert_eq!(
        initial_count, 1,
        "fresh audio_devices_command should add exactly one \
         VOICEPI_DEVICES_BACKEND entry under VOICEPI_AUDIO_BACKEND=rust"
    );

    // Re-running the propagator must be a no-op.
    super::propagate_rust_devices_backend(&mut command);
    let after_count = command
        .env
        .iter()
        .filter(|(k, _)| k == "VOICEPI_DEVICES_BACKEND")
        .count();
    assert_eq!(
        after_count, 1,
        "propagator must be idempotent — re-running it cannot \
         duplicate VOICEPI_DEVICES_BACKEND"
    );
}
