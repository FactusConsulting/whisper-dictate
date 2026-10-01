use super::*;

#[test]
fn start_blocked_by_a_transcript_action_advances_error_revision() {
    let mut app = test_app(AppSettings::default());
    let (_tx, rx) = mpsc::channel();
    app.background_task = Some(rx);
    app.background_task_label = Some(REINJECT_LAST_LABEL);
    let revision = app.runtime_error_revision;

    app.start_runtime();

    assert_eq!(app.runtime_error_revision, revision.wrapping_add(1));
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("Cannot start the runtime while a transcript action is running.")
    );
}

#[test]
fn start_is_blocked_while_the_local_nemotron_probe_is_active() {
    let mut app = test_app(AppSettings::default());
    app.nemotron_probe_active = Some(Arc::new(AtomicBool::new(true)));
    let revision = app.runtime_error_revision;

    app.start_runtime();

    assert_eq!(app.runtime_error_revision, revision.wrapping_add(1));
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("Cannot start the runtime while the local Nemotron model test is running.")
    );
    assert_eq!(app.runtime_state, crate::runtime::RuntimeState::Stopped);
}

#[test]
fn start_is_blocked_while_a_benchmark_is_running() {
    let mut app = test_app(AppSettings::default());
    let (_tx, rx) = mpsc::channel();
    app.background_task = Some(rx);
    app.background_task_label = Some(RUN_BENCHMARK_LABEL);
    let revision = app.runtime_error_revision;

    app.start_runtime();

    assert_eq!(app.runtime_error_revision, revision.wrapping_add(1));
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("Cannot start the runtime while a benchmark is running.")
    );
    assert_eq!(app.runtime_state, crate::runtime::RuntimeState::Stopped);
}

#[test]
fn lifecycle_gate_ignores_non_transcript_background_tasks() {
    let mut app = test_app(AppSettings::default());
    let (_tx, rx) = mpsc::channel();
    app.background_task = Some(rx);
    app.background_task_label = Some("doctor");

    assert!(!app.transcript_action_running());

    app.background_task_label = Some(REINJECT_LAST_LABEL);
    assert!(app.transcript_action_running());
}
