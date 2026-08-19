use super::in_process::InProcessInstallError;
use super::supervisor::{
    ascii_escape, install_error_stage, redacted_env_names, validate_engine_selection,
    RuntimeSupervisor,
};
use super::worker_command::WorkerCommand;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn command(program: &str, pairs: Vec<(String, String)>) -> WorkerCommand {
    WorkerCommand::from_runtime_pairs(
        PathBuf::from(program),
        Vec::new(),
        PathBuf::from("."),
        pairs,
    )
    .unwrap()
}

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
fn unknown_engine_values_are_ascii_escaped_for_windows_consoles() {
    let escaped = ascii_escape("rust-\u{1f525}");
    assert_eq!(escaped, "rust-\\u{1f525}");
    assert!(escaped.is_ascii());

    let error = validate_engine_selection(Some("rust-\u{1f525}"))
        .unwrap_err()
        .to_string();
    assert!(error.is_ascii());
    assert!(error.contains("\\u{1f525}"));
}

#[test]
fn every_native_start_failure_has_a_stable_diagnostic_stage() {
    let cases = [
        (InProcessInstallError::FeaturesMissing, "feature-check"),
        (
            InProcessInstallError::ConfigLoadFailed("bad config".into()),
            "config-load",
        ),
        (
            InProcessInstallError::InvalidOptions("unsupported cuda".into()),
            "runtime-options",
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

#[cfg(not(feature = "whisper-rs-vulkan"))]
#[test]
fn gui_start_rejects_effective_vulkan_before_installing_runtime_resources() {
    let _lock = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _device = super::test_support::EnvVarGuard::set("VOICEPI_DEVICE", "cpu");
    let _backend = super::test_support::EnvVarGuard::set("VOICEPI_STT_BACKEND", "whisper");
    let command = command(
        "legacy-worker-must-not-run",
        vec![
            ("VOICEPI_DEVICE".into(), "vulkan".into()),
            ("VOICEPI_STT_BACKEND".into(), "whisper".into()),
        ],
    );
    let mut supervisor = RuntimeSupervisor::new();

    let error = supervisor
        .attempt_in_process_start(&command)
        .expect_err("CPU-only GUI startup must reject an effective Vulkan request");

    assert!(matches!(
        error,
        InProcessInstallError::InvalidOptions(ref message)
            if message.contains("cannot honor device=vulkan")
    ));
    assert_eq!(install_error_stage(&error), "runtime-options");
    assert!(
        supervisor.hotkey_handle.is_none(),
        "validation must run before hotkey/audio/model resources are installed"
    );
}

#[test]
fn trace_metadata_exposes_names_but_never_secret_values() {
    let command = command(
        "whisper-dictate",
        vec![
            ("VOICEPI_STT_API_KEY".into(), "super-secret".into()),
            ("VOICEPI_MODEL".into(), "large-v3-turbo".into()),
        ],
    );
    let names = redacted_env_names(&command);
    assert!(names.contains(&"VOICEPI_STT_API_KEY".to_owned()));
    assert!(names.contains(&"VOICEPI_MODEL".to_owned()));
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
fn native_runtime_failures_use_the_persistent_diagnostic_sink() {
    let sources = [
        include_str!("../dictate/audio_ducking/mod.rs"),
        include_str!("../dictate/session/history_sink.rs"),
        include_str!("../dictate/session/metrics_sink.rs"),
        include_str!("../dictate/session/preview/engine.rs"),
        include_str!("../dictate/session/wire.rs"),
        include_str!("../dictate/backends/cloud_transcribe.rs"),
        include_str!("../dictate/backends/inject.rs"),
        include_str!("../dictate/backends/whisper_local.rs"),
    ];
    for source in sources {
        assert!(
            !source.contains("eprintln!"),
            "native session diagnostics must reach gui-diagnostic.log instead of raw stderr"
        );
        assert!(
            source.contains("crate::diag::log!"),
            "each guarded native module must route failures through the diagnostic sink"
        );
    }
}

#[test]
fn nix_package_launches_native_rust_without_python_payload() {
    let package = include_str!("../../../nix/package.nix");
    for retired in [
        "python3",
        "whisper_dictate.runtime",
        "src/python",
        "PYTHONPATH",
        "faster-whisper",
    ] {
        assert!(
            !package.contains(retired),
            "Nix production package must not retain retired Python flow {retired}"
        );
    }
    assert!(package.contains("rustPlatform.buildRustPackage"));
    assert!(package.contains("\"--no-default-features\""));
    assert!(package.contains("\"shipping\""));
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
fn teardown_task_never_blocks_the_controller_thread() {
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let started = Instant::now();
    let done = super::control::spawn_teardown_task(move || {
        release_rx.recv().expect("test releases teardown");
    });

    assert!(
        started.elapsed() < Duration::from_millis(100),
        "scheduling teardown must return before the blocking cleanup completes"
    );
    assert!(
        matches!(done.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "completion must not be reported while cleanup remains blocked"
    );
    release_tx.send(()).unwrap();
    done.recv_timeout(Duration::from_secs(2))
        .expect("teardown completion is reported");
}

#[test]
fn queued_restart_waits_for_teardown_completion_before_starting() {
    let _lock = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _engine = super::test_support::EnvVarGuard::set(super::in_process::ENGINE_ENV, "rust");
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.teardown_rx = Some(done_rx);
    supervisor.pending_restart = Some(command("legacy-worker-must-not-run", Vec::new()));
    supervisor.state = super::RuntimeState::Starting;

    assert!(supervisor.is_teardown_pending());
    let before = supervisor.poll();
    assert!(before.is_empty());
    assert_eq!(supervisor.state(), super::RuntimeState::Starting);
    assert!(
        supervisor.pending_restart.is_some(),
        "restart command must remain queued while teardown is incomplete"
    );

    done_tx.send(()).unwrap();
    let after = supervisor.poll();
    assert!(supervisor.pending_restart.is_none());
    assert_eq!(supervisor.state(), super::RuntimeState::Stopped);
    assert!(
        after
            .iter()
            .any(|event| matches!(event, super::RuntimeEvent::Error(_))),
        "completion must attempt the replacement start and report its stock-build failure"
    );
}

#[test]
fn restart_replaces_the_command_already_queued_behind_teardown() {
    // `restart` validates the process-wide engine selector while
    // materializing the replacement command. Serialize that read against
    // tests which temporarily set the retired selector.
    let _lock = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_done_tx, done_rx) = std::sync::mpsc::channel();
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.teardown_rx = Some(done_rx);
    supervisor.pending_restart = Some(command("old-settings", Vec::new()));
    supervisor.state = super::RuntimeState::Starting;
    let replacement = command(
        "new-settings",
        vec![("VOICEPI_AUDIO_DEVICE".to_owned(), "new-mic".to_owned())],
    );

    supervisor.restart(replacement.clone()).unwrap();

    assert_eq!(supervisor.pending_restart, Some(replacement));
    assert!(supervisor.is_running_or_restarting());
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
#[test]
fn polling_a_dead_hotkey_listener_stops_the_native_runtime() {
    let (handle, _tracker) = crate::hotkey::HotkeyHandle::install_stub_for_tests(
        crate::hotkey::coordinator::Mode::HoldToTalk,
        vec!["f9".to_owned()],
    );
    handle.mark_listener_dead_for_tests();
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.hotkey_handle = Some(handle);
    supervisor.state = super::RuntimeState::Running;

    let events = supervisor.poll();

    assert_eq!(supervisor.state(), super::RuntimeState::Stopped);
    assert!(events
        .iter()
        .any(|event| matches!(event, super::RuntimeEvent::Error(message) if message.contains("listener exited"))));
    assert!(events
        .iter()
        .any(|event| matches!(event, super::RuntimeEvent::Exited { code: Some(1) })));
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
fn stop_rejects_events_from_the_previous_runtime_generation() {
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.state = super::RuntimeState::Running;
    let stale_sender = supervisor.tx.clone();

    supervisor.stop().unwrap();

    assert!(stale_sender
        .send(super::RuntimeEvent::Worker(super::WorkerEvent {
            event: "status".to_owned(),
            state: Some("error".to_owned()),
            payload: serde_json::json!({"reason": "device_unusable"}),
        }))
        .is_err());
    let events = supervisor.poll();
    assert!(!events
        .iter()
        .any(|event| matches!(event, super::RuntimeEvent::Worker(_))));
    assert!(events
        .iter()
        .any(|event| matches!(event, super::RuntimeEvent::Exited { code: Some(0) })));
}

#[test]
fn restart_rejects_events_from_the_previous_runtime_generation() {
    let _lock = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _engine = super::test_support::EnvVarGuard::set(super::in_process::ENGINE_ENV, "rust");
    let (_done_tx, done_rx) = std::sync::mpsc::channel();
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.teardown_rx = Some(done_rx);
    supervisor.state = super::RuntimeState::Starting;
    let stale_sender = supervisor.tx.clone();

    supervisor
        .restart(command("replacement-runtime", Vec::new()))
        .unwrap();

    assert!(stale_sender
        .send(super::RuntimeEvent::Worker(super::WorkerEvent {
            event: "status".to_owned(),
            state: Some("error".to_owned()),
            payload: serde_json::json!({"reason": "device_unusable"}),
        }))
        .is_err());
    supervisor.send_event_for_tests(super::RuntimeEvent::Stdout("replacement event".to_owned()));
    assert_eq!(
        supervisor.poll(),
        vec![super::RuntimeEvent::Stdout("replacement event".to_owned())]
    );
}

#[test]
fn stop_closes_the_injection_gate_even_without_a_child_process() {
    let mut supervisor = RuntimeSupervisor::new();
    let active = Arc::new(AtomicBool::new(true));
    supervisor.runtime_active = Some(Arc::clone(&active));
    supervisor.state = super::RuntimeState::Running;

    supervisor.stop().unwrap();

    assert!(!active.load(Ordering::Acquire));
}

#[test]
fn stop_closes_audio_synchronously_before_reporting_stopped() {
    let mut supervisor = RuntimeSupervisor::new();
    let capture_closed = Arc::new(AtomicBool::new(false));
    let capture_closed_by_stop = Arc::clone(&capture_closed);
    supervisor.capture_stop = Some(Arc::new(move || {
        capture_closed_by_stop.store(true, Ordering::Release);
    }));
    supervisor.state = super::RuntimeState::Running;

    supervisor.stop().unwrap();

    assert!(
        capture_closed.load(Ordering::Acquire),
        "Stop must close CPAL before returning Stopped, even if coordinator teardown remains pending"
    );
    assert_eq!(supervisor.state(), super::RuntimeState::Stopped);
    assert!(supervisor.capture_stop.is_none());
}

#[test]
fn terminal_native_exit_transitions_to_stopped_and_closes_gate() {
    let mut supervisor = RuntimeSupervisor::new();
    let active = Arc::new(AtomicBool::new(true));
    supervisor.runtime_active = Some(Arc::clone(&active));
    supervisor.state = super::RuntimeState::Running;
    supervisor
        .tx
        .send(super::RuntimeEvent::Exited { code: Some(1) })
        .unwrap();

    let events = supervisor.poll();

    assert!(events
        .iter()
        .any(|event| matches!(event, super::RuntimeEvent::Exited { code: Some(1) })));
    assert_eq!(supervisor.state(), super::RuntimeState::Stopped);
    assert!(!active.load(Ordering::Acquire));
}

#[cfg(all(windows, feature = "rust-hotkeys", feature = "rust-injection"))]
#[test]
fn windows_controller_stop_drops_listener_before_reporting_stopped() {
    let _lock = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let dir = tempfile::tempdir().expect("temp config dir");
    let config = dir.path().join("config.json");
    std::fs::write(&config, r#"{"key":"f9"}"#).expect("write config");
    let _config =
        super::test_support::EnvVarGuard::set("VOICEPI_CONFIG", config.to_string_lossy().as_ref());
    let (handle, tracker) = crate::hotkey::HotkeyHandle::install_stub_for_tests(
        crate::hotkey::coordinator::Mode::HoldToTalk,
        vec!["f9".into()],
    );
    let active = Arc::new(AtomicBool::new(true));
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.hotkey_handle = Some(handle);
    supervisor.runtime_active = Some(Arc::clone(&active));
    supervisor.state = super::RuntimeState::Running;

    supervisor.stop().expect("controller stop");
    assert!(!active.load(Ordering::Acquire));
    assert!(
        supervisor.hotkey_handle.is_none(),
        "Stop must drop the native session so CPAL and backend resources close"
    );
    assert!(
        tracker.lock().unwrap().targets_for_tests().is_empty(),
        "Stop must unregister the listener tracker"
    );
}

#[test]
fn restart_path_rebuilds_instead_of_resuming_captured_dependencies() {
    let supervisor = include_str!("supervisor.rs");
    let control = include_str!("control.rs");
    assert!(!supervisor.contains("resume_key_names_from_env"));
    assert!(!supervisor.contains("resume-hotkey"));
    assert!(control.contains("self.hotkey_handle.take()"));
    assert!(control.contains("self.start(command)"));
}

#[test]
fn failed_restart_never_restores_a_retired_runtime() {
    let _lock = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _engine = super::test_support::EnvVarGuard::set(super::in_process::ENGINE_ENV, "python");
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.state = super::RuntimeState::Running;
    let command = command("legacy-worker-must-not-run", Vec::new());

    let error = supervisor.restart(command).unwrap_err();

    assert!(error.to_string().contains("no longer supported"));
    assert_eq!(supervisor.state(), super::RuntimeState::Stopped);
    assert!(!supervisor.is_running());
}
