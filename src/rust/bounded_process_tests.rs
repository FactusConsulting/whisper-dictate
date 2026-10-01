use super::{fixtures, run, Completion, DIAGNOSTIC_LIMIT};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn writer_closes_stdin_to_deliver_eof_before_waiting_for_exit() {
    let output = run(
        &mut fixtures::command("echo"),
        Some(b"utf8 input".to_vec()),
        Duration::from_secs(30),
        DIAGNOSTIC_LIMIT,
        &|| true,
    )
    .unwrap();
    assert_eq!(output.completion, Completion::Exited);
    assert!(output.status.success());
    assert!(output.stdout.ends_with(b"utf8 input"));
    assert!(output.stdin_error.is_none());
}

#[test]
fn nonreading_stdin_cannot_block_the_whole_execution_deadline() {
    let started = Instant::now();
    let output = run(
        &mut fixtures::command("sleep"),
        Some(vec![b'x'; 4 * 1024 * 1024]),
        Duration::from_millis(300),
        DIAGNOSTIC_LIMIT,
        &|| true,
    )
    .unwrap();
    assert_eq!(output.completion, Completion::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn cancellation_before_launch_never_spawns_the_program() {
    let error = run(
        &mut Command::new("missing-helper-that-must-not-launch"),
        None,
        Duration::from_secs(1),
        DIAGNOSTIC_LIMIT,
        &|| false,
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
}

#[test]
fn noisy_output_is_drained_without_unbounded_capture_or_deadlock() {
    let output = run(
        &mut fixtures::command("flood"),
        None,
        Duration::from_secs(30),
        4096,
        &|| true,
    )
    .unwrap();
    assert_eq!(output.completion, Completion::Exited);
    assert!(output.status.success());
    assert!(output.stdout_truncated);
    assert_eq!(output.stdout.len(), 4096);
    assert_eq!(output.stderr.len(), 4096);
    assert!(output.stdout.ends_with(b"stdout-finished"));
    assert!(output.stderr.ends_with(b"stderr-finished"));
}

#[test]
fn a_descendant_cannot_keep_pipes_open_after_its_parent_exits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("descendant.pid");
    let mut command = fixtures::command("descendant");
    command.env("WD_HELPER_TEST_DESCENDANT_FILE", &path);
    let started = Instant::now();
    let output = run(&mut command, None, Duration::from_secs(30), 4096, &|| true).unwrap();
    assert_eq!(output.completion, Completion::Exited);
    assert!(output.status.success());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!fixtures::is_running(fixtures::pid_file(&path)));
}

#[test]
fn cancellation_during_a_blocked_write_reaps_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("child.pid");
    let mut command = fixtures::command("sleep");
    command.env("WD_HELPER_TEST_PID_FILE", &path);
    let started = Instant::now();
    let output = run(
        &mut command,
        Some(vec![b'x'; 4 * 1024 * 1024]),
        Duration::from_secs(30),
        DIAGNOSTIC_LIMIT,
        &|| {
            std::fs::read_to_string(&path)
                .ok()
                .is_none_or(|pid| pid.parse::<u32>().is_err())
        },
    )
    .unwrap();
    assert_eq!(output.completion, Completion::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!fixtures::is_running(fixtures::pid_file(&path)));
}

#[test]
fn successful_clipboard_writer_preserves_its_background_owner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner.pid");
    let mut command = fixtures::command("clipboard-owner");
    command
        .env("WD_HELPER_TEST_DESCENDANT_FILE", &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let output =
        super::run_clipboard_owner(&mut command, b"selection".to_vec(), Duration::from_secs(30))
            .unwrap();
    assert_eq!(output.completion, Completion::Exited);
    assert!(output.status.success());
    let pid = fixtures::pid_file(&path);
    assert!(
        fixtures::is_running(pid),
        "a clipboard owner must survive successful publication"
    );
    let started = Instant::now();
    while fixtures::is_running(pid) {
        assert!(started.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
}
