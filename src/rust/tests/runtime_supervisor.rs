use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use whisper_dictate_app::runtime::{RuntimeState, RuntimeSupervisor, WorkerCommand};

fn engine_env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn unreachable_legacy_worker() -> WorkerCommand {
    WorkerCommand {
        program: PathBuf::from("legacy-python-worker-must-never-run.exe"),
        args: vec!["--sentinel".to_owned()],
        working_dir: PathBuf::from("Z:\\legacy-worker-path-must-not-exist"),
        env: vec![(
            "LEGACY_WORKER_SENTINEL".to_owned(),
            "secret-value".to_owned(),
        )],
    }
}

#[test]
fn retired_python_selector_is_rejected_before_worker_launch() {
    let _lock = engine_env_lock();
    let previous = std::env::var_os("VOICEPI_DICTATE_ENGINE");
    std::env::set_var("VOICEPI_DICTATE_ENGINE", "python");
    let mut supervisor = RuntimeSupervisor::new();

    let error = supervisor.start(unreachable_legacy_worker()).unwrap_err();

    match previous {
        Some(value) => std::env::set_var("VOICEPI_DICTATE_ENGINE", value),
        None => std::env::remove_var("VOICEPI_DICTATE_ENGINE"),
    }
    assert!(error
        .to_string()
        .contains("VOICEPI_DICTATE_ENGINE=python is no longer supported"));
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
}

#[test]
fn unknown_selector_is_rejected_before_worker_launch() {
    let _lock = engine_env_lock();
    let previous = std::env::var_os("VOICEPI_DICTATE_ENGINE");
    std::env::set_var("VOICEPI_DICTATE_ENGINE", "mojo");
    let mut supervisor = RuntimeSupervisor::new();

    let error = supervisor.start(unreachable_legacy_worker()).unwrap_err();

    match previous {
        Some(value) => std::env::set_var("VOICEPI_DICTATE_ENGINE", value),
        None => std::env::remove_var("VOICEPI_DICTATE_ENGINE"),
    }
    assert!(error.to_string().contains("unknown VOICEPI_DICTATE_ENGINE"));
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
}
