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
