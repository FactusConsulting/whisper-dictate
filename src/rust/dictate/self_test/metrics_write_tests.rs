//! Sibling regression tests for [`super::metrics_write`].
//!
//! See `feedback_tests.rs` for the sibling-test rationale.

use super::metrics_write::{run_metrics_write_self_test, MetricsWriteOptions, MetricsWriteReport};

#[test]
fn crate_public_api_surface_is_reachable_through_module() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metrics.jsonl");
    let opts = MetricsWriteOptions {
        text: "smoke".to_owned(),
        path_override: Some(path.clone()),
    };
    let report: MetricsWriteReport = run_metrics_write_self_test(opts);
    assert!(report.exit_ok());
    assert!(report.enabled);
    assert!(report.bytes_written > 0);
    assert!(path.exists());
}

#[test]
fn json_envelope_kind_is_stable() {
    let report = MetricsWriteReport {
        enabled: false,
        path: None,
        row: None,
        bytes_written: 0,
        error: None,
    };
    assert!(report.to_json().contains("\"metrics_write_self_test\""));
}
