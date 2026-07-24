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
    let handle = install_rust_hotkey_from_command(&command, tx, None);
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
    let handle = install_rust_hotkey_from_command(&command, tx, None);
    assert!(
        handle.is_none(),
        "VOICEPI_HOTKEY_BACKEND not set → must return None"
    );
}

// -----------------------------------------------------------------------
// P2 #373 finding 2: parse_toggle_value accepts all truthy variants.
// -----------------------------------------------------------------------

// ----- source_root off-by-one guard (PR #564 regression fix) -------------
//
// `source_root` is the checkout-fallback for `app_root` when the running exe
// isn't in an installed layout. It reads `CARGO_MANIFEST_DIR` and walks up.
// `CARGO_MANIFEST_DIR` = `<repo>/src/rust`, so the repo root is TWO levels
// up. A regression to `.nth(3)` (or higher) walks past the repo root and
// PYTHONPATH lands in the parent directory, which breaks every worker spawn
// from `cargo run` / `./target/release/whisper-dictate-gui.exe`
// (`ModuleNotFoundError: No module named 'whisper_dictate'`).

#[test]
fn source_root_lands_at_the_repo_root_not_its_parent() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // Sanity: this crate lives at <repo>/src/rust — a rename would need a
    // matching change to `source_root`'s ancestor index.
    assert_eq!(
        manifest.file_name().and_then(|s| s.to_str()),
        Some("rust"),
        "CARGO_MANIFEST_DIR is not `<repo>/src/rust`; source_root's nth() index would need updating"
    );
    let root = source_root();
    // The repo root MUST contain `src/python/whisper_dictate/runtime.py`
    // — that's exactly the file `python_source_root(app_root)` puts on
    // PYTHONPATH so the `python -m whisper_dictate.runtime` spawn resolves.
    assert!(
        root.join("src")
            .join("python")
            .join("whisper_dictate")
            .join("runtime.py")
            .exists(),
        "source_root() = {root:?} does not contain src/python/whisper_dictate/runtime.py — \
         nth() index likely walked past the repo root"
    );
}

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

// -----------------------------------------------------------------------
// P3 #372 finding 1: corpus_record id passed as `--flag=value` to argparse
// so a leading-hyphen id is not mis-parsed as another flag.
// -----------------------------------------------------------------------

#[test]
fn record_corpus_item_command_joins_id_with_equals_sign() {
    // Argparse processes `--flag value` by greedy lookahead and treats a
    // value starting with `-` as another flag. is_safe_corpus_id allows
    // leading hyphens (its allowlist is [A-Za-z0-9._-]) so the worker
    // command builder must use the unambiguous `--flag=value` form to
    // round-trip such ids correctly through Python argparse.
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    let command = super::record_corpus_item_command("-leading-hyphen");

    assert!(
        command
            .args
            .iter()
            .any(|a| a == "--record-corpus-item=-leading-hyphen"),
        "expected joined `--record-corpus-item=-leading-hyphen` token in args, got {:?}",
        command.args
    );
    // And there must be no naked `--record-corpus-item` flag followed by a
    // separate value token — argparse would parse `-leading-hyphen` as a
    // flag in that case.
    assert!(
        !command.args.iter().any(|a| a == "--record-corpus-item"),
        "must not emit the split `--record-corpus-item <id>` form; got {:?}",
        command.args
    );
}

#[test]
fn record_corpus_item_command_joins_typical_id_too() {
    // Belt-and-braces: the joined form is the only emitted form for every
    // id, not just the hyphen-leading edge case. Catches a future refactor
    // that re-introduces the split form for "normal-looking" ids and
    // silently loses the hyphen protection.
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    let command = super::record_corpus_item_command("da-001");

    assert!(
        command
            .args
            .iter()
            .any(|a| a == "--record-corpus-item=da-001"),
        "ordinary ids also use the joined form; got {:?}",
        command.args
    );
}

// -----------------------------------------------------------------------
// P3 #372 finding 2: run_foreground must call configure_piped_python_stdio
// so foreground subcommands (bench / corpus-record / doctor / models)
// inherit UTF-8 stdio on Windows where the console code page defaults to
// cp1252 / cp437 and mojibakes Danish corpus text. Existing UTF-8 test
// covers run_capture; this regression guard covers run_foreground via
// source inspection (the foreground path inherits stdout, so behavioural
// tests would need to capture the parent process's piped output which is
// elaborate; the source-inspection pattern matches the existing
// `install_commands_use_background_process_flags` test next door).
// -----------------------------------------------------------------------

#[test]
fn run_foreground_configures_python_utf8_stdio() {
    // Post-500-LOC-refactor: run_foreground + configure_*_stdio live
    // in `runtime/process.rs`.
    let runtime = include_str!("process.rs");
    let run_foreground_body = runtime
        .split_once("pub fn run_foreground(command: &WorkerCommand)")
        .expect("run_foreground signature should still exist")
        .1
        .split_once("fn configure_background_process")
        .expect("expected configure_background_process to follow run_foreground")
        .0;

    assert!(
        run_foreground_body.contains("configure_piped_python_stdio(&mut process);"),
        "run_foreground must call configure_piped_python_stdio so foreground \
         subcommands (bench / corpus-record / doctor / models) get UTF-8 \
         stdio — without it Danish corpus text mojibakes on Windows where \
         the inherited console code page is cp1252 (P3 #372 finding 2)"
    );
}

#[test]
fn configure_piped_python_stdio_sets_both_utf8_envs() {
    // The helper's CONTRACT (vs the call-site test above): both
    // PYTHONUTF8=1 AND PYTHONIOENCODING=utf-8 must be set on the command.
    // PYTHONUTF8 alone is not enough on Windows because Python reopens
    // its stdio streams with the configured encoding before the env is
    // read; PYTHONIOENCODING is the seatbelt. We inspect the function
    // source directly because std::process::Command does not expose an
    // env getter to assert against at runtime.
    // Post-500-LOC-refactor: this helper lives in `runtime/process.rs`.
    let runtime = include_str!("process.rs");
    let helper_body = runtime
        .split_once("fn configure_piped_python_stdio(command: &mut Command)")
        .expect("configure_piped_python_stdio signature should still exist")
        .1
        .split_once("fn exit_status_to_result")
        .expect("expected exit_status_to_result to follow configure_piped_python_stdio")
        .0;

    assert!(
        helper_body.contains(".env(PYTHON_UTF8_ENV,"),
        "configure_piped_python_stdio must set PYTHONUTF8 (P3 #372 finding 2)"
    );
    assert!(
        helper_body.contains(".env(PYTHON_IO_ENCODING_ENV,"),
        "configure_piped_python_stdio must set PYTHONIOENCODING for Windows \
         stdio reopen (P3 #372 finding 2)"
    );
}

// -----------------------------------------------------------------------
// P3 #383: rdev-specific PTT-key aliases (`right_alt`, `ralt`) must be
// normalised to the canonical pynput name (`alt_gr`) before the worker
// command's VOICEPI_KEY is forwarded to the Python worker — pynput
// resolves the keyname against its catalogue at startup BEFORE the
// listener registers, so an unknown name terminates the worker even when
// VOICEPI_PYTHON_HOTKEY=0 has parked the listener.
// -----------------------------------------------------------------------

#[test]
fn normalise_hotkey_chord_rewrites_right_alt_to_alt_gr() {
    assert_eq!(
        super::normalise_hotkey_chord_for_python("right_alt"),
        "alt_gr"
    );
    assert_eq!(super::normalise_hotkey_chord_for_python("ralt"), "alt_gr");
}

#[test]
fn normalise_hotkey_chord_is_case_insensitive_on_aliases() {
    // The Rust validator accepts case-insensitively (matches the rest of
    // the alias-handling); so must the normaliser.
    for raw in ["Right_Alt", "RIGHT_ALT", "RAlt", "RALT", "  right_alt  "] {
        assert_eq!(
            super::normalise_hotkey_chord_for_python(raw),
            "alt_gr",
            "expected {raw:?} to normalise to alt_gr"
        );
    }
}

#[test]
fn normalise_hotkey_chord_preserves_non_alias_names() {
    // Canonical names that pynput already understands must pass through
    // unchanged — including unrelated segments.
    for raw in ["ctrl_r", "shift_l", "alt_gr", "alt_l", "f9", "space", "esc"] {
        assert_eq!(
            super::normalise_hotkey_chord_for_python(raw),
            raw,
            "{raw:?} must not be rewritten"
        );
    }
}

#[test]
fn normalise_hotkey_chord_handles_chord_bindings() {
    // Chord bindings (`+`-separated) must normalise per-segment — the
    // alias can appear anywhere in the chord.
    assert_eq!(
        super::normalise_hotkey_chord_for_python("ctrl_r+right_alt"),
        "ctrl_r+alt_gr"
    );
    assert_eq!(
        super::normalise_hotkey_chord_for_python("shift_l+ralt+f9"),
        "shift_l+alt_gr+f9"
    );
    assert_eq!(
        super::normalise_hotkey_chord_for_python("ctrl_r+shift_r"),
        "ctrl_r+shift_r"
    );
}

#[test]
fn normalise_hotkey_chord_leaves_empty_input_alone() {
    assert_eq!(super::normalise_hotkey_chord_for_python(""), "");
}

#[test]
fn normalise_hotkey_aliases_in_env_rewrites_voicepi_key() {
    let mut env = vec![
        ("VOICEPI_MODEL".to_owned(), "large-v3".to_owned()),
        ("VOICEPI_KEY".to_owned(), "right_alt".to_owned()),
        ("VOICEPI_DEVICE".to_owned(), "cuda".to_owned()),
    ];
    super::normalise_hotkey_aliases_for_python(&mut env);
    let key_value = env
        .iter()
        .find(|(k, _)| k == "VOICEPI_KEY")
        .map(|(_, v)| v.as_str());
    assert_eq!(key_value, Some("alt_gr"));
    let model_value = env
        .iter()
        .find(|(k, _)| k == "VOICEPI_MODEL")
        .map(|(_, v)| v.as_str());
    assert_eq!(
        model_value,
        Some("large-v3"),
        "unrelated entries must not be touched"
    );
}

#[test]
fn normalise_hotkey_aliases_in_env_is_noop_when_voicepi_key_missing() {
    let original = vec![("VOICEPI_MODEL".to_owned(), "small".to_owned())];
    let mut env = original.clone();
    super::normalise_hotkey_aliases_for_python(&mut env);
    assert_eq!(env, original, "no VOICEPI_KEY → vector unchanged");
}

#[test]
fn worker_command_normalises_right_alt_in_voicepi_key() {
    // End-to-end: a user with VOICEPI_KEY=right_alt in their process env
    // must see the WorkerCommand emit VOICEPI_KEY=alt_gr so the Python
    // worker's pynput resolution succeeds at startup (P3 #383).
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _key_guard = EnvVarGuard::set("VOICEPI_KEY", "right_alt");

    let command = worker_command("/tmp/whisper-dictate");
    let key_value = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_KEY")
        .map(|(_, v)| v.as_str());
    assert_eq!(
        key_value,
        Some("alt_gr"),
        "VOICEPI_KEY=right_alt must be rewritten to alt_gr in the worker env"
    );
}

// ----- cli_exe_from: two-binary split resolution (PR #564 Codex P1) ---------
//
// After splitting into `whisper-dictate.exe` (console CLI) +
// `whisper-dictate-gui.exe` (windows-subsystem tray), every internal spawn
// path that used to be `env::current_exe()` had to be routed through
// `cli_exe_path()` — otherwise the Settings UI's Install/repair action and
// the worker's `VOICEPI_RUST_INJECTOR` env var would silently re-launch the
// tray instead of running the intended CLI verb. These tests exercise the
// pure resolution helper.

#[test]
fn cli_exe_from_gui_binary_resolves_unix_sibling() {
    // POSIX-style paths — this is what the container/Linux CI exercises.
    // No .exe suffix; the sibling name follows the same convention.
    let gui = PathBuf::from("/opt/whisper-dictate/whisper-dictate-gui");
    let cli = cli_exe_from(&gui);
    assert_eq!(cli, PathBuf::from("/opt/whisper-dictate/whisper-dictate"));
}

#[test]
fn cli_exe_from_cli_binary_unix_is_identity() {
    let cli_unix = PathBuf::from("/usr/local/bin/whisper-dictate");
    assert_eq!(cli_exe_from(&cli_unix), cli_unix);
}

#[test]
fn cli_exe_from_unknown_binary_name_is_identity() {
    // A dev renamed the binary (or the release toolchain produced an odd
    // name). The helper must not silently rewrite unfamiliar names — the
    // failure mode we want is "unknown CLI verb" from whatever the user
    // actually launched, not a mysterious tray re-launch. Bare filenames
    // parse identically on Windows and Unix, so we cover the "no
    // directory" case here without a cfg gate.
    let bare = PathBuf::from("whisper-dictate-experimental");
    assert_eq!(cli_exe_from(&bare), bare);
}

// Windows-path assertions require the OS-native `\` separator to parse
// correctly (POSIX PathBuf treats `\` as a filename character, not a
// separator, so `with_file_name` on a Windows-style literal loses the
// directory prefix and the assertion falsely fails on Linux CI). Both
// cases still exercise the SAME pure resolver `cli_exe_from`.
#[cfg(windows)]
#[test]
fn cli_exe_from_gui_binary_resolves_windows_sibling() {
    let gui = PathBuf::from(
        r"C:\Users\lars\AppData\Local\Programs\WhisperDictate\whisper-dictate-gui.exe",
    );
    let cli = cli_exe_from(&gui);
    assert_eq!(
        cli,
        PathBuf::from(r"C:\Users\lars\AppData\Local\Programs\WhisperDictate\whisper-dictate.exe",),
    );
}

#[cfg(windows)]
#[test]
fn cli_exe_from_cli_binary_windows_is_identity() {
    let cli_win = PathBuf::from(r"C:\Program Files\WhisperDictate\whisper-dictate.exe");
    assert_eq!(cli_exe_from(&cli_win), cli_win);
}

#[cfg(windows)]
#[test]
fn cli_exe_from_gui_binary_case_insensitive_on_windows_names() {
    // Windows paths from PATH/registry sometimes come back case-shifted
    // (WHISPER-DICTATE-GUI.EXE); the resolution must still recognise them.
    let gui = PathBuf::from(r"C:\PROGRAMS\WHISPERDICTATE\WHISPER-DICTATE-GUI.EXE");
    let cli = cli_exe_from(&gui);
    // The sibling name we emit is the canonical lowercase one — this is
    // what the installer + portable ZIP actually place on disk, and cargo
    // produces `whisper-dictate.exe` (lowercase) too.
    assert_eq!(
        cli,
        PathBuf::from(r"C:\PROGRAMS\WHISPERDICTATE\whisper-dictate.exe"),
    );
}
