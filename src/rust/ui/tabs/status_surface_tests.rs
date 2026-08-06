use super::super::*;
use super::status_surface::{compact_status_state, CompactStatus};

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
        compact_status_state(RuntimeState::Running, true, Some("injecting"), false),
        CompactStatus::Injecting
    );
}

#[test]
fn status_surface_error_overrides_pipeline_state() {
    assert_eq!(
        compact_status_state(RuntimeState::Running, true, Some("recording"), true),
        CompactStatus::Error
    );
}
