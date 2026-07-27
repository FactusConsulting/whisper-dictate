//! Sibling regression tests for [`super::preview`].
//!
//! See `feedback_tests.rs` for the sibling-test rationale.

use std::time::Duration;

use super::preview::{run_preview_self_test, CannedPreviewBackend, PreviewOptions, PreviewReport};
use crate::dictate::PreviewBackend;

#[test]
fn crate_public_api_surface_is_reachable_through_module() {
    // Default options MUST produce at least one emission — that's the
    // "engine booted, ticked, delivered" pass signal.
    let opts = PreviewOptions::default();
    let report: PreviewReport = run_preview_self_test(opts);
    assert!(report.exit_ok());
    assert!(!report.emissions.is_empty());
}

#[test]
fn canned_backend_reports_call_count() {
    let backend = CannedPreviewBackend::new("hi");
    let _ = backend.transcribe_partial(&[], 16_000);
    let _ = backend.transcribe_partial(&[], 16_000);
    assert_eq!(backend.calls(), 2);
}

#[test]
fn json_envelope_kind_is_stable() {
    let report = PreviewReport {
        frames_pushed: 0,
        frame_samples: 0,
        sample_rate: 16_000,
        interval: Duration::from_millis(100),
        emissions: Vec::new(),
        error: None,
    };
    assert!(report.to_json().contains("\"preview_self_test\""));
}
