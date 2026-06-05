use std::env;
#[cfg(windows)]
use std::ffi::{OsStr, OsString};
#[cfg(windows)]
use std::fs;
use std::path::PathBuf;
#[cfg(windows)]
use std::process::Child;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use whisper_dictate_app::runtime::cleanup_stale_desktop_processes;
use whisper_dictate_app::runtime::{RuntimeEvent, RuntimeState, RuntimeSupervisor, WorkerCommand};

#[test]
fn supervisor_captures_stdout_and_exit() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "print('worker-ready', flush=True)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| {
        has_stdout(events, "worker-ready") && has_exit(events)
    });

    assert!(has_stdout(&events, "worker-ready"));
    assert!(has_exit(&events));
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
}

#[test]
fn supervisor_stops_running_process() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "import time; print('worker-ready', flush=True); time.sleep(30)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| has_stdout(events, "worker-ready"));
    assert!(has_stdout(&events, "worker-ready"));
    assert_eq!(supervisor.state(), RuntimeState::Running);

    supervisor.stop().unwrap();
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
    let events = collect_until(&mut supervisor, has_exit);
    assert!(has_exit(&events));
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
}

#[test]
fn supervisor_stop_returns_without_waiting_for_process_exit_event() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "import time; print('worker-ready', flush=True); time.sleep(30)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| has_stdout(events, "worker-ready"));
    assert!(has_stdout(&events, "worker-ready"));

    let started = Instant::now();
    supervisor.stop().unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "stop should return immediately instead of waiting for process teardown"
    );
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
}

#[test]
fn supervisor_restart_returns_without_waiting_for_process_exit_event() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python.clone(),
            args: vec![
                "-c".to_owned(),
                "import time; print('worker-ready', flush=True); time.sleep(30)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| has_stdout(events, "worker-ready"));
    assert!(has_stdout(&events, "worker-ready"));

    let started = Instant::now();
    supervisor
        .restart(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "print('worker-restarted', flush=True)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "restart should return immediately instead of waiting for process teardown"
    );

    let events = collect_until(&mut supervisor, |events| {
        has_stdout(events, "worker-restarted")
    });
    assert!(has_stdout(&events, "worker-restarted"));
}

#[test]
fn supervisor_parses_worker_events_from_stderr() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "import os, sys; assert os.environ['VOICEPI_WORKER_EVENTS'] == '1'; print('[worker-event] {\"event\":\"status\",\"state\":\"ready\"}', file=sys.stderr, flush=True)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| {
        has_worker_status(events, "ready") && has_exit(events)
    });

    assert!(has_worker_status(&events, "ready"));
    assert!(!events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Stderr(line) if line.starts_with("[worker-event]")
    )));
}

#[test]
fn supervisor_passes_command_env_to_worker_without_logging_secret() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "import os; print(len(os.environ.get('VOICEPI_TEST_SECRET', '')) == 12, flush=True)"
                    .to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: vec![("VOICEPI_TEST_SECRET".to_owned(), "secret-value".to_owned())],
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| {
        has_stdout(events, "True") && has_exit(events)
    });

    assert!(has_stdout(&events, "True"));
    assert!(!events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Started { command } if command.contains("secret-value")
    )));
}

#[cfg(windows)]
#[test]
fn cleanup_stale_desktop_processes_stops_worker_from_same_app_root() {
    let Some(python) = test_python() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let package = dir
        .path()
        .join("src")
        .join("python")
        .join("whisper_dictate");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("__init__.py"), "").unwrap();
    let worker = package.join("runtime.py");
    fs::write(
        &worker,
        "import time\nprint('worker-ready', flush=True)\ntime.sleep(60)\n",
    )
    .unwrap();
    let root_arg = dir.path().display().to_string();
    let mut child = Command::new(python)
        .args(["-m", "whisper_dictate.runtime", "--app-root", &root_arg])
        .current_dir(dir.path())
        .env("PYTHONPATH", dir.path().join("src").join("python"))
        .spawn()
        .unwrap();
    let _app_root_guard = EnvVarGuard::set("VOICEPI_APP_ROOT", dir.path());

    cleanup_stale_desktop_processes();

    assert!(
        wait_for_exit(&mut child, Duration::from_secs(5)),
        "stale worker should be stopped by startup cleanup"
    );
}

fn test_python() -> Option<PathBuf> {
    for candidate in python_candidates() {
        if Command::new(&candidate)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return Some(PathBuf::from(candidate));
        }
    }
    None
}

#[cfg(windows)]
fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    false
}

#[cfg(windows)]
struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

#[cfg(windows)]
impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(key);
        env::set_var(key, value);
        Self { key, original }
    }
}

#[cfg(windows)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            env::set_var(self.key, value);
        } else {
            env::remove_var(self.key);
        }
    }
}

fn python_candidates() -> &'static [&'static str] {
    if cfg!(windows) {
        &["py.exe", "py", "python.exe", "python"]
    } else {
        &["python3", "python"]
    }
}

fn collect_until(
    supervisor: &mut RuntimeSupervisor,
    predicate: impl Fn(&[RuntimeEvent]) -> bool,
) -> Vec<RuntimeEvent> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    while Instant::now() < deadline {
        events.extend(supervisor.poll());
        if predicate(&events) {
            return events;
        }
        thread::sleep(Duration::from_millis(25));
    }
    events
}

fn has_stdout(events: &[RuntimeEvent], expected: &str) -> bool {
    events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Stdout(line) if line == expected))
}

fn has_exit(events: &[RuntimeEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Exited { .. }))
}

fn has_worker_status(events: &[RuntimeEvent], expected: &str) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            RuntimeEvent::Worker(worker)
                if worker.event == "status" && worker.state.as_deref() == Some(expected)
        )
    })
}
