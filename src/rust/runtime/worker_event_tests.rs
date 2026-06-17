use super::*;

#[test]
fn parses_worker_event_lines() {
    let event = parse_worker_event(
        r#"[worker-event] {"event":"status","state":"ready","model":"large-v3"}"#,
    )
    .unwrap();

    assert_eq!(event.event, "status");
    assert_eq!(event.state.as_deref(), Some("ready"));
    assert_eq!(event.payload["model"], "large-v3");
}

#[test]
fn invalid_worker_event_lines_fall_back_to_stderr() {
    assert!(parse_worker_event("[worker-event] not json").is_none());
    assert!(parse_worker_event("ordinary stderr").is_none());
}

#[test]
fn repaint_notifier_is_off_by_default() {
    let supervisor = RuntimeSupervisor::new();
    assert!(!supervisor.has_repaint_notifier());
}

#[test]
fn set_repaint_notifier_flips_has_repaint_notifier_to_true() {
    use std::sync::Arc;
    let mut supervisor = RuntimeSupervisor::new();
    assert!(!supervisor.has_repaint_notifier());
    supervisor.set_repaint_notifier(Arc::new(|| {}));
    assert!(supervisor.has_repaint_notifier());
}
