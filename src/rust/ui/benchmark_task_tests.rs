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

// A multi-line stdout sample: a scored row, a skipped-no-audio row, and the
// trailing `[benchmark]` summary line — the shape the worker emits.
const BENCHMARK_STDOUT: &str = "whisper-dictate 1.12.0
{\"event\":\"benchmark_result\",\"benchmark_success\":true,\"benchmark_skipped\":false,\"corpus_id\":\"da-001\",\"corpus_language\":\"da\",\"wer\":0.1,\"cer\":0.05}
{\"event\":\"benchmark_result\",\"benchmark_success\":false,\"benchmark_skipped\":true,\"benchmark_error\":\"audio file missing\",\"corpus_id\":\"da-002\",\"corpus_language\":\"da\",\"wer\":1.0,\"cer\":1.0}
[benchmark] 1/2 passed, 1 skipped (no audio), avg WER 10.0%, avg CER 5.0%
";

fn benchmark_result(success: bool) -> BackgroundTaskResult {
    BackgroundTaskResult {
        label: RUN_BENCHMARK_LABEL,
        command: "py --run-benchmark".to_owned(),
        stdout: BENCHMARK_STDOUT.to_owned(),
        stderr: String::new(),
        success,
        code: Some(if success { 0 } else { 1 }),
        error: None,
    }
}

#[test]
fn apply_benchmark_results_populates_the_parsed_model_from_stdout() {
    let mut app = test_app(AppSettings::default());
    app.apply_benchmark_results(&benchmark_result(true));

    let results = app
        .benchmark_results
        .as_ref()
        .expect("benchmark_results should be populated on completion");
    assert_eq!(results.summary.total, 2);
    assert_eq!(results.summary.scored, 1);
    assert_eq!(results.summary.skipped, 1);
    // Average WER is over the SCORED row only (0.1), not the skipped row's 1.0.
    assert!((results.summary.avg_wer.unwrap() - 0.1).abs() < 1e-6);
    assert_eq!(results.rows.len(), 2);

    // The raw JSONL is still streamed to the runtime log (behaviour preserved),
    // and the concise `[benchmark] …` summary line still lands as the `[OK]`
    // detail — the digestible view is purely additive.
    assert!(
        app.runtime_log.contains("\"corpus_id\":\"da-001\""),
        "raw per-item JSONL must remain in the log, got: {}",
        app.runtime_log
    );
    assert!(
        app.runtime_log
            .contains("[OK] run benchmark passed: [benchmark] 1/2 passed"),
        "the concise summary line must remain in the log, got: {}",
        app.runtime_log
    );
}

#[test]
fn run_benchmark_clears_previous_results_when_a_new_run_starts() {
    let mut app = test_app(AppSettings::default());
    // Seed a prior parsed result, as if a previous run had completed.
    app.apply_benchmark_results(&benchmark_result(true));
    assert!(app.benchmark_results.is_some());

    // Starting a fresh run (no other task in flight) must clear the stale model.
    app.run_benchmark();
    assert!(
        app.benchmark_results.is_none(),
        "a new run must clear the previous parsed results"
    );
}

#[test]
fn gated_run_keeps_previous_results_visible() {
    // When the run is GATED (another task in flight) the prior results must stay
    // on screen — only an actually-starting run clears them.
    let mut app = test_app(AppSettings::default());
    app.apply_benchmark_results(&benchmark_result(true));
    let (_tx, rx) = mpsc::channel::<BackgroundTaskResult>();
    app.background_task = Some(rx);
    app.background_task_label = Some("install/repair");

    app.run_benchmark();

    assert!(
        app.benchmark_results.is_some(),
        "a gated run must leave the previous results visible"
    );
}

#[test]
fn apply_benchmark_results_surfaces_run_failure_and_clears_model() {
    let mut app = test_app(AppSettings::default());
    app.apply_benchmark_results(&benchmark_result(true));
    assert!(app.benchmark_results.is_some());

    // A run that couldn't start (no stdout) clears the stale model and logs it.
    let failed = BackgroundTaskResult {
        label: RUN_BENCHMARK_LABEL,
        command: "py --run-benchmark".to_owned(),
        stdout: String::new(),
        stderr: String::new(),
        success: false,
        code: None,
        error: Some("worker could not start".to_owned()),
    };
    app.apply_benchmark_results(&failed);

    assert!(app.benchmark_results.is_none());
    assert!(
        app.runtime_log.contains("worker could not start"),
        "the run failure must be logged, got: {}",
        app.runtime_log
    );
}

#[test]
fn apply_benchmark_results_on_nonzero_exit_still_parses_rows() {
    // A non-zero exit can still carry usable per-item rows — show them, and log
    // the failure with the summary line as detail.
    let mut app = test_app(AppSettings::default());
    app.apply_benchmark_results(&benchmark_result(false));

    assert!(app.benchmark_results.is_some());
    assert_eq!(app.benchmark_results.as_ref().unwrap().summary.total, 2);
    assert!(
        app.runtime_log
            .contains("[ERROR] run benchmark failed with code 1"),
        "non-zero exit must be logged, got: {}",
        app.runtime_log
    );
}
