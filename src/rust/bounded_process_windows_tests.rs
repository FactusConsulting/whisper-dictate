use super::{process_handles, spawn, ProcessTree};
use crate::bounded_process::fixtures;
use std::io;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// Records the census `terminate` takes but sends no kill, so the helper's own
/// exit stands in for a teardown that Windows has not finished yet.
fn pending_teardown(role: &str) -> (Child, ProcessTree) {
    let mut command = fixtures::command(role);
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let (child, mut tree) = spawn(&mut command).unwrap();
    tree.terminating_processes = process_handles(tree.job.0).unwrap();
    assert_eq!(tree.terminating_processes.len(), 1);
    (child, tree)
}

#[test]
fn teardown_slower_than_a_second_is_awaited_to_completion() {
    let (mut child, tree) = pending_teardown("slow-exit");
    let started = Instant::now();
    tree.wait_terminated().unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert!(child.try_wait().unwrap().is_some());
}

#[test]
fn a_tree_that_never_finishes_terminating_errors_at_the_bound() {
    let (mut child, tree) = pending_teardown("sleep");
    let grace = Duration::from_millis(300);
    let started = Instant::now();
    let error = tree.wait_terminated_within(grace).unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(elapsed >= grace, "{elapsed:?}");
    // Only scheduling slack on a loaded machine; the fixture lives for 60s.
    assert!(elapsed < grace + Duration::from_secs(5), "{elapsed:?}");
    // Closing the job kills the fixture.
    drop(tree);
    child.wait().unwrap();
}
