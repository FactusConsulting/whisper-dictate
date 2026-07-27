//! Sibling regression tests for [`super::feedback`].
//!
//! The bulk of the unit tests live inline in `feedback.rs` next to the
//! runner. This sibling file exists so the regression-test discipline
//! scanner (`src/tests/python/test_regression_test_discipline.py`) sees
//! a matching test file for the new self-test module, and pins the
//! crate-public API surface the CLI dispatcher in `main.rs` calls
//! through.

use std::time::Duration;

use super::feedback::{resolve_backend, run_feedback_self_test, FeedbackOptions, FeedbackReport};

#[test]
fn crate_public_api_surface_is_reachable_through_module() {
    // Pin the public symbols the CLI dispatcher in `main.rs` uses. If any
    // one of these is renamed / moved the dispatcher's `use` statement
    // AND this test both break together — which is the discipline the
    // scanner enforces.
    let opts = FeedbackOptions::default();
    let report: FeedbackReport = run_feedback_self_test(opts);
    // The runner MUST attempt both cues (the trait contract is
    // infallible) regardless of the resolved backend.
    assert!(report.start_played);
    assert!(report.stop_played);
}

#[test]
fn resolve_backend_returns_one_of_the_documented_labels() {
    // Smoke script pins these tokens — a rename must be an intentional
    // API break, not a drive-by refactor.
    let backend = resolve_backend();
    assert!(
        matches!(backend, "kernel32_beep" | "paplay" | "pw-play" | "noop"),
        "unexpected backend label {backend:?}"
    );
}

#[test]
fn zero_delay_options_are_infallible() {
    // A 0 ms delay is the CI-fast path — the runner must still exercise
    // both `play(Start)` and `play(Stop)` calls without sleeping.
    let opts = FeedbackOptions {
        delay: Duration::from_millis(0),
    };
    let report = run_feedback_self_test(opts);
    assert!(report.start_played && report.stop_played);
    assert_eq!(report.delay, Duration::from_millis(0));
}
