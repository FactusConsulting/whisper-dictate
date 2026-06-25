//! Iteration-2 review finding #3 regression tests.
//!
//! When the audio bridge surfaces a terminal `BridgeError::Io` or
//! `BridgeError::Pipeline`, the writer thread has already dropped the
//! child's stdin handle and exited. The supervisor MUST treat that as
//! a fatal worker failure — kill the child, drop the bridge, surface
//! an `Exited` event so the UI flips to Stopped — instead of leaving
//! a half-dead worker that can no longer receive audio.
//!
//! The watcher itself runs on a background thread and cannot mutate
//! `&mut self`; the contract is "watcher raises a flag, next `poll()`
//! performs the teardown". These tests pin that contract by toggling
//! the flag via the crate-visible test hook and then asserting `poll`
//! does the right thing.
//!
//! Only built with the `audio-in-rust` cargo feature because the flag
//! itself only exists in that build (it's `#[cfg(feature = "audio-in-rust")]`).

#![cfg(feature = "audio-in-rust")]

use super::*;
use crate::runtime::test_support::ENV_LOCK;
use std::process::Command;
use std::time::{Duration, Instant};

/// Locate the system Python (same probe shape as the existing
/// integration tests). Returns `None` when no Python is on PATH;
/// callers should treat that as "skip the test" rather than fail, so
/// hermetic dev environments without Python don't break CI.
fn test_python() -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(windows) {
        &["py.exe", "py", "python.exe", "python"]
    } else {
        &["python3", "python"]
    };
    for candidate in candidates {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
        {
            return Some(PathBuf::from(candidate));
        }
    }
    None
}

fn collect_until(
    supervisor: &mut RuntimeSupervisor,
    pred: impl Fn(&[RuntimeEvent]) -> bool,
    deadline: Duration,
) -> Vec<RuntimeEvent> {
    let stop = Instant::now() + deadline;
    let mut events = Vec::new();
    while Instant::now() < stop {
        events.extend(supervisor.poll());
        if pred(&events) {
            return events;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    events
}

#[test]
fn terminal_bridge_flag_kills_child_and_surfaces_exited_on_next_poll() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(python) = test_python() else {
        return;
    };
    // A long-lived worker so we can verify the supervisor's poll-driven
    // teardown actually terminates it (rather than racing a self-exit).
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "import time; print('worker-ready', flush=True); time.sleep(60)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .expect("supervisor start");

    // Wait until the worker reports ready so we know the child is
    // genuinely alive when we trigger the teardown.
    let initial = collect_until(
        &mut supervisor,
        |events| {
            events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::Stdout(line) if line == "worker-ready"))
        },
        Duration::from_secs(5),
    );
    assert!(
        initial
            .iter()
            .any(|e| matches!(e, RuntimeEvent::Stdout(line) if line == "worker-ready")),
        "worker must reach ready state before the test triggers the bridge failure"
    );
    assert!(
        supervisor.is_running(),
        "worker must be running pre-trigger"
    );

    // Simulate the bridge watcher observing BridgeError::Io. The next
    // poll() must perform the iteration-2 finding #3 teardown.
    supervisor.trigger_bridge_terminal_for_tests();

    let post = collect_until(
        &mut supervisor,
        |events| {
            events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::Exited { .. }))
        },
        Duration::from_secs(5),
    );
    assert!(
        post.iter()
            .any(|e| matches!(e, RuntimeEvent::Exited { .. })),
        "finding #3: terminal bridge error must synthesize an Exited event; got: {post:?}",
    );
    assert!(
        !supervisor.is_running(),
        "finding #3: worker child must be killed after terminal bridge error",
    );
    assert_eq!(
        supervisor.state(),
        RuntimeState::Stopped,
        "finding #3: state must flip to Stopped so the UI updates",
    );
}

#[test]
fn terminal_bridge_flag_without_child_just_resets_state() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Pre-start state. The watcher can race against an already-exited
    // worker (child died → poll() saw it first → next iteration sees
    // the bridge flag). The teardown branch must be safe in that case
    // — no panic, no spurious extra Exited event, state stays Stopped.
    let mut supervisor = RuntimeSupervisor::new();
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
    supervisor.trigger_bridge_terminal_for_tests();
    let events = supervisor.poll();
    assert_eq!(
        supervisor.state(),
        RuntimeState::Stopped,
        "state must stay Stopped when there is no child to kill",
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::Exited { .. })),
        "finding #3: no spurious Exited when there was no child to begin with; got: {events:?}",
    );
}
