use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

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

fn test_python() -> Option<PathBuf> {
    for candidate in python_candidates() {
        if Command::new(&candidate).arg("--version").output().is_ok() {
            return Some(PathBuf::from(candidate));
        }
    }
    None
}

fn python_candidates() -> &'static [&'static str] {
    if cfg!(windows) {
        &["python.exe", "python"]
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
