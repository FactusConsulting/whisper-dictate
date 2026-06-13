//! Tests for the System tab "Record corpus item" background-task wiring.
//!
//! The Record button drives `run_record_corpus_item`, which is gated on the
//! dictation runtime being STOPPED (recording must never disturb the managed
//! runtime — they would fight over the microphone) AND no other background task
//! running. These exercise ONLY the gated branches and the pure
//! `can_record_corpus_item` predicate, so no Python worker is ever spawned.

use super::corpus_record_tasks::RECORD_CORPUS_ITEM_LABEL;
use super::test_support::test_app;
use super::*;
use crate::runtime::record_corpus_item_command;
use std::sync::mpsc;

fn app_with_selection() -> WhisperDictateApp {
    let mut app = test_app(AppSettings::default());
    // A selected corpus item is the precondition for recording. The corpus load
    // is skipped (no disk) — we set the selection directly.
    app.corpus_loaded = true;
    app.corpus_items = vec![CorpusItem {
        id: "da-001".to_owned(),
        text: "Hej med dig.".to_owned(),
        language: "da".to_owned(),
    }];
    app.corpus_selected_id = Some("da-001".to_owned());
    app
}

fn item(id: &str) -> CorpusItem {
    CorpusItem {
        id: id.to_owned(),
        text: format!("text for {id}"),
        language: "da".to_owned(),
    }
}

/// An app with a 3-item corpus and no recordings, runtime stopped, no task — the
/// precondition for starting a batch. The corpus load is skipped (no disk).
fn app_with_corpus() -> WhisperDictateApp {
    let mut app = test_app(AppSettings::default());
    app.corpus_loaded = true;
    app.corpus_items = vec![item("a"), item("b"), item("c")];
    app
}

/// A synthetic `corpus_record_done` background result for `id` (no worker spawned).
fn done_result(id: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        label: RECORD_CORPUS_ITEM_LABEL,
        command: format!("py --record-corpus-item {id}"),
        stdout: format!(
            "{{\"event\":\"corpus_record_done\",\"id\":\"{id}\",\"path\":\"/a/{id}.wav\",\"seconds_recorded\":3.0}}\n"
        ),
        stderr: String::new(),
        success: true,
        code: Some(0),
        error: None,
    }
}

#[test]
fn record_is_skipped_while_the_runtime_is_running() {
    let mut app = app_with_selection();
    // The managed dictation runtime owns the microphone — recording must not run.
    app.runtime_state = RuntimeState::Running;

    app.run_record_corpus_item();

    assert!(app.background_task.is_none(), "no task should start");
    assert!(
        app.runtime_log.contains("skipped"),
        "expected a skip notice, got: {}",
        app.runtime_log
    );
    assert!(!app.can_record_corpus_item());
}

#[test]
fn record_is_skipped_while_another_background_task_runs() {
    let mut app = app_with_selection();
    let (_tx, rx) = mpsc::channel::<BackgroundTaskResult>();
    app.background_task = Some(rx);
    app.background_task_label = Some("install/repair");

    app.run_record_corpus_item();

    // The record run must NOT have replaced the running task.
    assert_eq!(app.background_task_label, Some("install/repair"));
    assert!(
        app.runtime_log.contains("skipped"),
        "expected a skip notice, got: {}",
        app.runtime_log
    );
    assert!(!app.can_record_corpus_item());
}

#[test]
fn record_does_nothing_without_a_selected_item() {
    let mut app = test_app(AppSettings::default());
    app.corpus_selected_id = None;

    app.run_record_corpus_item();

    assert!(app.background_task.is_none());
    assert!(!app.can_record_corpus_item());
}

#[test]
fn can_record_requires_selection_stopped_and_idle() {
    let mut app = app_with_selection();
    // All preconditions met (stopped runtime, no task, an item selected).
    assert!(app.can_record_corpus_item());

    // Drop the selection → cannot record.
    app.corpus_selected_id = None;
    assert!(!app.can_record_corpus_item());
}

#[test]
fn command_construction_passes_record_corpus_item_with_id() {
    let command = record_corpus_item_command("da-001");
    let display = command.display();
    assert!(
        display.contains("--record-corpus-item"),
        "missing flag in: {display}"
    );
    assert!(display.contains("da-001"), "missing id in: {display}");
}

#[test]
fn label_is_stable() {
    // The label is the dispatch key the poll handler routes on; pin it so a
    // rename is a conscious, test-visible change.
    assert_eq!(RECORD_CORPUS_ITEM_LABEL, "record corpus item");
}

#[test]
fn apply_corpus_record_parses_done_into_inline_result() {
    let mut app = app_with_selection();
    let result = BackgroundTaskResult {
        label: RECORD_CORPUS_ITEM_LABEL,
        command: "py --record-corpus-item da-001".to_owned(),
        stdout: "{\"event\":\"corpus_record_done\",\"id\":\"da-001\",\"path\":\"/a/da-001.wav\",\"seconds_recorded\":9.8,\"peak_dbfs\":-6.0}\n".to_owned(),
        stderr: String::new(),
        success: true,
        code: Some(0),
        error: None,
    };

    app.apply_corpus_record(&result);

    match app.corpus_record_result {
        Some(Ok(CorpusRecordOutcome::Saved { id, path, .. })) => {
            assert_eq!(id, "da-001");
            assert_eq!(path, "/a/da-001.wav");
        }
        other => panic!("expected Saved outcome, got {other:?}"),
    }
}

#[test]
fn apply_corpus_record_parses_error_into_inline_result() {
    let mut app = app_with_selection();
    let result = BackgroundTaskResult {
        label: RECORD_CORPUS_ITEM_LABEL,
        command: "py --record-corpus-item bad".to_owned(),
        stdout: "{\"event\":\"corpus_record_error\",\"error\":\"unknown corpus id: bad\"}\n"
            .to_owned(),
        stderr: String::new(),
        success: true,
        code: Some(0),
        error: None,
    };

    app.apply_corpus_record(&result);

    match app.corpus_record_result {
        Some(Ok(CorpusRecordOutcome::Failed { error })) => {
            assert!(error.contains("unknown corpus id"), "{error}");
        }
        other => panic!("expected Failed outcome, got {other:?}"),
    }
}

#[test]
fn apply_corpus_record_surfaces_run_failure_as_err() {
    let mut app = app_with_selection();
    let result = BackgroundTaskResult {
        label: RECORD_CORPUS_ITEM_LABEL,
        command: "py --record-corpus-item da-001".to_owned(),
        stdout: String::new(),
        stderr: String::new(),
        success: false,
        code: None,
        error: Some("worker could not start".to_owned()),
    };

    app.apply_corpus_record(&result);

    assert!(
        matches!(app.corpus_record_result, Some(Err(ref msg)) if msg.contains("worker could not start")),
        "expected an Err outcome, got {:?}",
        app.corpus_record_result
    );
}

// ── Batch recording ──────────────────────────────────────────────────────────

#[test]
fn can_start_batch_requires_stopped_idle_and_no_batch() {
    let mut app = app_with_corpus();
    assert!(app.can_start_corpus_batch(), "stopped + idle + no batch");

    // Runtime running → cannot start (it owns the mic).
    app.runtime_state = RuntimeState::Running;
    assert!(!app.can_start_corpus_batch());
    app.runtime_state = RuntimeState::Stopped;

    // A background task in flight → cannot start.
    let (_tx, rx) = mpsc::channel::<BackgroundTaskResult>();
    app.background_task = Some(rx);
    assert!(!app.can_start_corpus_batch());
    app.background_task = None;

    // Already inside a batch → cannot start another.
    app.corpus_batch = CorpusBatch::new(vec!["a".to_owned()]);
    assert!(!app.can_start_corpus_batch());
}

#[test]
fn start_batch_all_missing_with_everything_recorded_is_a_no_op() {
    let mut app = app_with_corpus();
    // Mark every item as already recorded → "all missing" has nothing to do.
    app.corpus_recorded_ids = ["a", "b", "c"].iter().map(|s| (*s).to_owned()).collect();

    app.start_corpus_batch(BatchScope::AllMissing);

    assert!(!app.corpus_batch_active(), "no batch should start");
    assert!(app.background_task.is_none(), "no worker should launch");
    assert!(
        app.runtime_log.contains("nothing to record"),
        "expected a no-op breadcrumb, got: {}",
        app.runtime_log
    );
}

#[test]
fn start_batch_while_runtime_running_does_not_start() {
    let mut app = app_with_corpus();
    app.runtime_state = RuntimeState::Running;

    app.start_corpus_batch(BatchScope::All);

    assert!(!app.corpus_batch_active());
    assert!(app.background_task.is_none());
}

#[test]
fn batch_advances_to_the_next_item_on_a_done_event() {
    // Drive the sequential advance WITHOUT spawning a worker: seed the cursor
    // directly, then feed a synthetic done-event through apply_corpus_record.
    let mut app = app_with_corpus();
    app.corpus_batch = CorpusBatch::new(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);

    // First clip ("a") done → cursor advances to "b", a resume gap is armed.
    app.apply_corpus_record(&done_result("a"));
    let batch = app.corpus_batch.as_ref().expect("batch still active");
    assert_eq!(batch.current(), Some("b"));
    assert_eq!(batch.completed(), 1);
    assert!(
        app.corpus_batch_resume_at.is_some(),
        "the inter-clip gap should be armed for the next launch"
    );
}

#[test]
fn batch_finishes_and_clears_after_the_last_item() {
    let mut app = app_with_corpus();
    app.corpus_batch = CorpusBatch::new(vec!["only".to_owned()]);

    app.apply_corpus_record(&done_result("only"));

    assert!(
        !app.corpus_batch_active(),
        "the batch should clear once the last item is done"
    );
    assert!(app.corpus_batch_resume_at.is_none());
    assert!(
        app.runtime_log.contains("complete"),
        "expected a completion log, got: {}",
        app.runtime_log
    );
}

#[test]
fn stop_batch_ends_the_run_and_clears_the_gap() {
    let mut app = app_with_corpus();
    app.corpus_batch = CorpusBatch::new(vec!["a".to_owned(), "b".to_owned()]);
    app.corpus_batch_resume_at = Some(std::time::Instant::now());

    app.stop_corpus_batch();

    assert!(!app.corpus_batch_active(), "Stop ends the run");
    assert!(app.corpus_batch_resume_at.is_none());
    assert!(
        app.runtime_log.contains("stopped by user"),
        "expected a stop log, got: {}",
        app.runtime_log
    );
}

#[test]
fn batch_aborts_when_a_clip_reports_a_failure() {
    let mut app = app_with_corpus();
    app.corpus_batch = CorpusBatch::new(vec!["a".to_owned(), "b".to_owned()]);

    // A worker-reported failure (not a crash) on the first clip stops the batch
    // rather than looping the same failure across the remaining items.
    let failed = BackgroundTaskResult {
        label: RECORD_CORPUS_ITEM_LABEL,
        command: "py --record-corpus-item a".to_owned(),
        stdout: "{\"event\":\"corpus_record_error\",\"error\":\"no audio was captured\"}\n"
            .to_owned(),
        stderr: String::new(),
        success: true,
        code: Some(0),
        error: None,
    };
    app.apply_corpus_record(&failed);

    assert!(!app.corpus_batch_active(), "a failed clip ends the batch");
    assert!(app.corpus_batch_resume_at.is_none());
}

#[test]
fn poll_batch_stops_the_run_if_the_runtime_starts_mid_batch() {
    // Defensive: a batch must never wedge if the runtime is (re)started while it
    // is waiting out the inter-clip gap.
    let mut app = app_with_corpus();
    app.corpus_batch = CorpusBatch::new(vec!["a".to_owned(), "b".to_owned()]);
    app.corpus_batch_resume_at = Some(std::time::Instant::now());
    app.runtime_state = RuntimeState::Running;

    app.poll_corpus_batch();

    assert!(
        !app.corpus_batch_active(),
        "the batch should stop when the runtime is no longer stopped"
    );
}

#[test]
fn single_item_record_does_not_create_a_batch() {
    // A done-event with no active batch (the single Record path) must not start
    // or leave any batch state behind.
    let mut app = app_with_corpus();
    assert!(!app.corpus_batch_active());

    app.apply_corpus_record(&done_result("a"));

    assert!(
        !app.corpus_batch_active(),
        "single-item path stays batch-free"
    );
    assert!(app.corpus_batch_resume_at.is_none());
}
