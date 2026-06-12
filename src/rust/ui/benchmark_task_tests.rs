//! Tests for the System tab "Run benchmark" background-task wiring.
//!
//! The button drives `run_benchmark`, which routes through the shared
//! `run_background_command` gate. The gate is the load-bearing safety property:
//! the (slow) benchmark must never be launched while another background task is
//! already running. We exercise ONLY the gated branch here, so no Python worker
//! is ever spawned — the gate returns early, making this a pure-logic test.

use super::tasks::RUN_BENCHMARK_LABEL;
use super::test_support::test_app;
use super::*;
use std::sync::mpsc;

#[test]
fn run_benchmark_is_skipped_while_another_background_task_runs() {
    let mut app = test_app(AppSettings::default());

    // Simulate an in-flight background task (e.g. a doctor/install run).
    let (_tx, rx) = mpsc::channel::<BackgroundTaskResult>();
    app.background_task = Some(rx);
    app.background_task_label = Some("install/repair");

    app.run_benchmark();

    // The benchmark must NOT have replaced the running task...
    assert_eq!(app.background_task_label, Some("install/repair"));
    assert!(app.background_task.is_some());
    // ...and the user is told it was skipped.
    assert!(
        app.runtime_log.contains("skipped"),
        "expected a skip notice in the log, got: {}",
        app.runtime_log
    );
    // ...and NO "benchmark started" line was emitted (the run never started).
    assert!(
        !app.runtime_log.contains("benchmark started"),
        "no start line should be logged when the run is gated, got: {}",
        app.runtime_log
    );
}

#[test]
fn run_benchmark_logs_immediate_start_line_when_it_starts() {
    // The model load + corpus pass is slow, so the button prints an immediate
    // "benchmark started" line (before the worker even spawns) so it never feels
    // dead. No other task is running here, so the run starts and the line lands.
    let mut app = test_app(AppSettings::default());

    app.run_benchmark();

    assert!(
        app.runtime_log
            .contains("[ui] benchmark started — results appear here when finished"),
        "expected an immediate start line, got: {}",
        app.runtime_log
    );
}

#[test]
fn run_benchmark_label_is_stable() {
    // The label is the dispatch key the generic poll handler logs and that this
    // test (and any future result-routing) keys on; pin it so a rename is a
    // conscious, test-visible change.
    assert_eq!(RUN_BENCHMARK_LABEL, "run benchmark");
}
