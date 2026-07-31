use super::in_process::InProcessInstallError;
use super::supervisor::{
    install_error_stage, redacted_env_names, validate_engine_selection, RuntimeSupervisor,
};
use super::worker_command::WorkerCommand;
use std::path::PathBuf;

#[test]
fn retired_and_unknown_engine_values_fail_with_migration_guidance() {
    assert!(validate_engine_selection(None).is_ok());
    assert!(validate_engine_selection(Some(" rust ")).is_ok());

    let retired = validate_engine_selection(Some("Python")).unwrap_err();
    assert!(retired.to_string().contains("no longer supported"));
    assert!(retired.to_string().contains("remove the variable"));

    let unknown = validate_engine_selection(Some("go")).unwrap_err();
    assert!(unknown
        .to_string()
        .contains("unknown VOICEPI_DICTATE_ENGINE"));
    assert!(!unknown.to_string().contains("fallback"));
}

#[test]
fn every_native_start_failure_has_a_stable_diagnostic_stage() {
    let cases = [
        (InProcessInstallError::FeaturesMissing, "feature-check"),
        (
            InProcessInstallError::ConfigLoadFailed("bad config".into()),
            "config-load",
        ),
        (InProcessInstallError::EmptyChord, "hotkey-config"),
        (
            InProcessInstallError::MissingBackend("missing model".into()),
            "backend-build",
        ),
        (
            InProcessInstallError::HotkeyInstallFailed("hook".into()),
            "hotkey-install",
        ),
        (
            InProcessInstallError::PttAlreadyHeld("owned".into()),
            "ptt-ownership",
        ),
        (
            InProcessInstallError::Panicked("panic".into()),
            "panic-boundary",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(install_error_stage(&error), expected);
        assert!(!error.to_string().contains("falling back"));
    }
}

#[test]
fn trace_metadata_exposes_names_but_never_secret_values() {
    let command = WorkerCommand {
        program: PathBuf::from("whisper-dictate"),
        args: vec!["run".into()],
        working_dir: PathBuf::from("."),
        env: vec![
            ("VOICEPI_STT_API_KEY".into(), "super-secret".into()),
            ("VOICEPI_MODEL".into(), "large-v3-turbo".into()),
        ],
    };
    let names = redacted_env_names(&command);
    assert_eq!(names, ["VOICEPI_STT_API_KEY", "VOICEPI_MODEL"]);
    assert!(!format!("{names:?}").contains("super-secret"));
}

#[test]
fn supervisor_and_terminal_have_no_python_spawn_or_fallback_flow() {
    let supervisor = include_str!("supervisor.rs");
    let terminal = include_str!("terminal_run.rs");
    for (name, source) in [("supervisor", supervisor), ("terminal", terminal)] {
        assert!(
            !source.contains("Command::new("),
            "{name} still spawns a child"
        );
        assert!(
            !source.contains("default_worker_command"),
            "{name} still constructs the retired worker"
        );
        assert!(
            !source.contains("run_foreground"),
            "{name} still calls the retired worker runner"
        );
        assert!(
            !source.to_ascii_lowercase().contains("fallback to python"),
            "{name} still documents a Python fallback"
        );
    }
}

#[test]
fn retired_audio_and_stdin_bridge_modules_cannot_reenter_the_build() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "audio/stdin_bridge.rs",
        "audio/pipe.rs",
        "runtime/audio_bridge.rs",
        "runtime/hotkey_install.rs",
        "tests/audio_stdin_bridge.rs",
    ] {
        assert!(
            !manifest.join(relative).exists(),
            "retired bridge file still exists: {relative}"
        );
    }

    let audio_modules = include_str!("../audio/mod.rs");
    let runtime_modules = include_str!("mod.rs");
    for retired in ["stdin_bridge", "event_to_json_line", "write_events"] {
        assert!(
            !audio_modules.contains(retired),
            "audio module graph still exposes retired symbol {retired}"
        );
    }
    assert!(
        !runtime_modules.contains("mod audio_bridge"),
        "runtime module graph still compiles the retired child bridge"
    );
    assert!(
        !runtime_modules.contains("mod hotkey_install"),
        "runtime module graph still compiles the retired Python hotkey bridge"
    );
}

#[test]
fn supervisor_starts_stopped_without_a_child_runtime_slot() {
    let supervisor = RuntimeSupervisor::new();
    assert!(!supervisor.is_running());
    assert_eq!(supervisor.state().label(), "Stopped");
}

#[test]
fn clean_shutdown_is_idempotent_and_emits_one_exit() {
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.state = super::RuntimeState::Running;

    supervisor.stop().unwrap();
    supervisor.stop().unwrap();

    assert_eq!(supervisor.state(), super::RuntimeState::Stopped);
    let exits = supervisor
        .poll()
        .into_iter()
        .filter(|event| matches!(event, super::RuntimeEvent::Exited { code: Some(0) }))
        .count();
    assert_eq!(exits, 1);
}

#[test]
fn failed_restart_never_restores_a_retired_runtime() {
    let _lock = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _engine = super::test_support::EnvVarGuard::set(super::in_process::ENGINE_ENV, "python");
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.state = super::RuntimeState::Running;
    let command = WorkerCommand {
        program: PathBuf::from("legacy-worker-must-not-run"),
        args: Vec::new(),
        working_dir: PathBuf::from("."),
        env: Vec::new(),
    };

    let error = supervisor.restart(command).unwrap_err();

    assert!(error.to_string().contains("no longer supported"));
    assert_eq!(supervisor.state(), super::RuntimeState::Stopped);
    assert!(!supervisor.is_running());
}
