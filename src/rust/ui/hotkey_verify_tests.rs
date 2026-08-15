use super::*;

#[test]
fn reducer_requires_press_and_release_in_the_expected_focus_context() {
    let mut report =
        HotkeyVerificationReport::new("pause".to_owned(), "win_registerhotkey".to_owned());
    assert!(!report.observe(HotkeyVerificationSignal::Press, Some(true)));
    assert!(!report.observe(HotkeyVerificationSignal::Release, Some(false)));
    assert_eq!(report.other_window, HotkeyVerificationOutcome::Untested);

    assert!(report.observe(HotkeyVerificationSignal::Press, Some(false)));
    assert!(report.observe(HotkeyVerificationSignal::Release, Some(false)));
    assert_eq!(report.other_window, HotkeyVerificationOutcome::Passed);
    assert_eq!(
        report.current_context(),
        Some(HotkeyFocusContext::WhisperDictate)
    );
}

#[test]
fn reducer_reports_focused_and_unfocused_results_separately() {
    let mut report = HotkeyVerificationReport::new("pause".to_owned(), "rdev".to_owned());
    assert!(report.fail_current());
    assert_eq!(report.other_window, HotkeyVerificationOutcome::Failed);
    assert_eq!(report.whisper_dictate, HotkeyVerificationOutcome::Untested);

    assert!(report.observe(HotkeyVerificationSignal::Press, Some(true)));
    assert!(report.observe(HotkeyVerificationSignal::Release, Some(true)));
    assert!(report.is_complete());
    assert!(!report.is_verified());
    assert_eq!(report.whisper_dictate, HotkeyVerificationOutcome::Passed);
}

#[test]
fn focus_change_between_press_and_release_does_not_create_a_false_pass() {
    let mut report = HotkeyVerificationReport::new("pause".to_owned(), "rdev".to_owned());
    assert!(report.observe(HotkeyVerificationSignal::Press, Some(false)));
    assert!(!report.observe(HotkeyVerificationSignal::Release, Some(true)));
    assert_eq!(report.other_window, HotkeyVerificationOutcome::Untested);
}

#[test]
fn unknown_viewport_focus_never_counts_as_verification() {
    let mut report = HotkeyVerificationReport::new("pause".to_owned(), "evdev".to_owned());
    assert!(!report.observe(HotkeyVerificationSignal::Press, None));
    assert!(!report.observe(HotkeyVerificationSignal::Release, None));
    assert_eq!(report.other_window, HotkeyVerificationOutcome::Untested);
}

#[test]
fn synthetic_session_observes_chord_events_without_process_env_mutation() {
    let (mut session, tx) = HotkeyVerificationSession::synthetic("pause", "test-stub");
    tx.send(HotkeyVerificationSignal::Press).unwrap();
    tx.send(HotkeyVerificationSignal::Release).unwrap();
    assert!(session.poll(Some(false)));
    assert_eq!(
        session.report().other_window,
        HotkeyVerificationOutcome::Passed
    );
    assert_eq!(session.report().driver, "test-stub");
}

#[test]
fn completed_result_is_bound_to_the_chord_that_was_tested() {
    let report = HotkeyVerificationReport::new("ctrl+f9".to_owned(), "rdev".to_owned());
    assert!(report.belongs_to("ctrl+f9"));
    assert!(!report.belongs_to("pause"));
}

#[test]
fn context_and_outcome_labels_are_actionable() {
    assert_eq!(
        HotkeyFocusContext::OtherWindow.label(),
        "Another focused window"
    );
    assert_eq!(
        HotkeyFocusContext::WhisperDictate.label(),
        "WhisperDictate focused"
    );
    assert_eq!(HotkeyVerificationOutcome::Untested.label(), "not tested");
    assert_eq!(HotkeyVerificationOutcome::Passed.label(), "verified");
    assert_eq!(
        HotkeyVerificationOutcome::Failed.label(),
        "failed / no response"
    );
}

#[test]
fn synthetic_session_can_mark_the_current_context_failed_and_shutdown() {
    let (mut session, _tx) = HotkeyVerificationSession::synthetic("pause", "test-stub");
    assert!(session.fail_current());
    assert_eq!(
        session.report().other_window,
        HotkeyVerificationOutcome::Failed
    );
    session.shutdown();
}

#[cfg(not(feature = "rust-hotkeys"))]
#[test]
fn reduced_build_session_start_returns_feature_error_without_side_effects() {
    let repaint: crate::runtime::RepaintNotifier = std::sync::Arc::new(|| {});
    let result = HotkeyVerificationSession::start("pause", repaint);
    assert!(matches!(result, Err(reason) if reason.contains("rust-hotkeys feature")));
}
