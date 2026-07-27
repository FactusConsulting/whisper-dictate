//! Sibling regression tests for [`super::history_write`].
//!
//! See `feedback_tests.rs` for the sibling-test rationale.

use super::history_write::{run_history_write_self_test, HistoryWriteOptions, HistoryWriteReport};

#[test]
fn crate_public_api_surface_is_reachable_through_module() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let opts = HistoryWriteOptions {
        text: "smoke".to_owned(),
        path_override: Some(path.clone()),
        force_enabled: Some(true),
    };
    let report: HistoryWriteReport = run_history_write_self_test(opts);
    assert!(report.exit_ok(), "smoke write must land on scratch dir");
    assert!(report.bytes_written > 0);
    assert!(path.exists());
}

#[test]
fn disabled_gate_still_reports_ok_with_no_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let opts = HistoryWriteOptions {
        text: "ignored".to_owned(),
        path_override: Some(path.clone()),
        force_enabled: Some(false),
    };
    let report = run_history_write_self_test(opts);
    assert!(report.exit_ok());
    assert!(!report.enabled);
    assert!(!path.exists(), "disabled gate must not touch disk");
}

#[test]
fn json_envelope_kind_is_stable() {
    let report = HistoryWriteReport {
        enabled: false,
        path: std::path::PathBuf::from("/tmp/history.jsonl"),
        row: None,
        bytes_written: 0,
        error: None,
    };
    assert!(report.to_json().contains("\"history_write_self_test\""));
}
