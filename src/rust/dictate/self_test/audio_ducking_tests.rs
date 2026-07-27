//! Sibling regression tests for [`super::audio_ducking`].
//!
//! See `feedback_tests.rs` for the sibling-test rationale (regression-test
//! discipline scanner + crate-public API pinning).

use std::time::Duration;

use super::audio_ducking::{
    resolve_backend, run_audio_ducking_self_test, AudioDuckingOptions, AudioDuckingReport,
};

#[test]
fn crate_public_api_surface_is_reachable_through_module() {
    let opts = AudioDuckingOptions {
        duration: Duration::from_millis(0),
        force_enabled: Some(false),
        force_level: Some(0.25),
    };
    let report: AudioDuckingReport = run_audio_ducking_self_test(opts);
    assert!(report.entered);
    assert!(report.exited);
    assert!(report.exit_ok(), "forced-disabled run must pass");
}

#[test]
fn backend_label_is_from_documented_set() {
    // The smoke script pins these three labels — a rename must trip the
    // scanner AND this assertion together.
    assert!(matches!(
        resolve_backend(),
        "wasapi" | "unsupported_platform" | "feature_disabled"
    ));
}

#[test]
fn json_envelope_kind_is_stable() {
    // The smoke script greps for `kind":"audio_ducking_self_test"` so a
    // rename here would silently kill the check.
    let report = AudioDuckingReport {
        backend: resolve_backend(),
        env_enabled: false,
        target_volume: 0.25,
        duration: Duration::from_millis(0),
        entered: true,
        exited: true,
        error: None,
    };
    assert!(report.to_json().contains("\"audio_ducking_self_test\""));
}
