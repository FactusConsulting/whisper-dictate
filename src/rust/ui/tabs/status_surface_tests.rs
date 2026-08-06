use super::super::test_support::test_app;
use super::super::*;
use super::status_surface::{compact_status_state, transcript_actions_enabled, CompactStatus};

#[test]
fn status_surface_state_covers_idle_start_and_pipeline() {
    assert_eq!(
        compact_status_state(RuntimeState::Stopped, false, None, false),
        CompactStatus::Idle
    );
    assert_eq!(
        compact_status_state(RuntimeState::Running, false, None, false),
        CompactStatus::Starting
    );
    assert_eq!(
        compact_status_state(RuntimeState::Running, true, Some("recording"), false),
        CompactStatus::Recording
    );
    assert_eq!(
        compact_status_state(RuntimeState::Running, true, Some("transcribing"), false),
        CompactStatus::Transcribing
    );
    assert_eq!(
        compact_status_state(RuntimeState::Running, true, Some("post-processing"), false),
        CompactStatus::PostProcessing
    );
    assert_eq!(
        compact_status_state(RuntimeState::Running, true, Some("injecting"), false),
        CompactStatus::Injecting
    );
}

#[test]
fn status_surface_error_is_shown_when_no_pipeline_is_active() {
    assert_eq!(
        compact_status_state(RuntimeState::Running, true, None, true),
        CompactStatus::Error
    );
}

#[test]
fn active_pipeline_state_takes_precedence_over_a_retained_error() {
    assert_eq!(
        compact_status_state(RuntimeState::Running, true, Some("recording"), true),
        CompactStatus::Recording
    );
}

#[test]
fn compact_metadata_uses_the_configured_local_model() {
    let app = test_app(AppSettings {
        stt_backend: "whisper".to_owned(),
        model: "small.en".to_owned(),
        ..Default::default()
    });
    assert_eq!(app.compact_metadata_model(), "small.en");
}

#[test]
fn transcript_actions_require_an_idle_task_lane() {
    assert!(transcript_actions_enabled(None, false));
    assert!(!transcript_actions_enabled(Some("injecting"), false));
    assert!(!transcript_actions_enabled(None, true));
}
