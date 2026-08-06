use super::super::test_support::test_app;
use super::super::*;
use super::status_surface::{
    compact_status_state, retained_target_available, target_activation_available_for,
    transcript_actions_enabled, CompactStatus,
};

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
fn compact_metadata_uses_saved_runtime_settings_while_running() {
    let mut app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        model: "pending-model".to_owned(),
        ..Default::default()
    });
    app.saved_settings.stt_backend = "whisper".to_owned();
    app.saved_settings.model = "large-v3".to_owned();
    app.runtime_state = RuntimeState::Running;

    assert_eq!(app.compact_metadata_model(), "large-v3");
}

#[test]
fn transcript_actions_require_an_idle_task_lane() {
    assert!(transcript_actions_enabled(None, false, "type", true));
    assert!(!transcript_actions_enabled(
        Some("injecting"),
        false,
        "type",
        true
    ));
    assert!(!transcript_actions_enabled(None, true, "type", true));
    assert!(!transcript_actions_enabled(None, false, "print", true));
    assert!(!transcript_actions_enabled(None, false, "type", false));
}

#[test]
fn pure_wayland_has_no_reinject_target_activation() {
    assert!(!target_activation_available_for(true, false));
    assert!(target_activation_available_for(true, true));
    assert!(target_activation_available_for(false, false));
}

#[test]
fn transcript_actions_require_a_retained_target() {
    assert!(!retained_target_available("", "", ""));
    assert!(retained_target_available("42", "", ""));
    assert!(retained_target_available("", "Editor", ""));
}
