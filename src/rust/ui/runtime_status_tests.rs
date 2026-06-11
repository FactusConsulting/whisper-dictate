//! Tests for the derived runtime-display status (don't show "Running" until the
//! worker has loaded the model) and the push-to-talk hotkey readout.

use super::tabs::{format_push_to_talk_keys, push_to_talk_badge_label};
use super::test_support::test_app;
use super::*;

fn status_event(state: &str) -> WorkerEvent {
    WorkerEvent {
        event: "status".to_owned(),
        state: Some(state.to_owned()),
        payload: serde_json::json!({ "event": "status", "state": state }),
    }
}

#[test]
fn display_state_stays_starting_until_worker_reports_ready() {
    let mut app = test_app(AppSettings::default());
    // The OS process has spawned, but the model is still loading.
    app.runtime_state = RuntimeState::Running;
    assert!(!app.worker_ready);
    assert_eq!(app.display_runtime_state(), RuntimeState::Starting);

    // The worker announces it is loading the model — still not ready.
    app.update_worker_status(&status_event("loading_model"));
    assert!(!app.worker_ready);
    assert_eq!(app.display_runtime_state(), RuntimeState::Starting);

    // Model loaded: now the stack can receive speech and we show Running.
    app.update_worker_status(&status_event("ready"));
    assert!(app.worker_ready);
    assert_eq!(app.display_runtime_state(), RuntimeState::Running);
}

#[test]
fn display_state_passes_through_stopped_and_keeps_running_once_ready() {
    let mut app = test_app(AppSettings::default());
    // Stopped is never rewritten to Starting.
    app.runtime_state = RuntimeState::Stopped;
    app.worker_ready = false;
    assert_eq!(app.display_runtime_state(), RuntimeState::Stopped);

    // Once ready, in-pipeline states keep the worker marked ready (so the badge
    // stays "Running" through recording/transcribing/post-processing).
    app.runtime_state = RuntimeState::Running;
    for state in ["opening", "recording", "transcribing", "post-processing"] {
        app.update_worker_status(&status_event(state));
        assert!(app.worker_ready, "{state} should keep worker ready");
        assert_eq!(app.display_runtime_state(), RuntimeState::Running);
    }
}

#[test]
fn failed_model_load_drops_back_to_starting() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    app.update_worker_status(&status_event("ready"));
    assert!(app.worker_ready);

    // A failed (re)load means we're no longer ready to receive speech.
    app.update_worker_status(&status_event("failed"));
    assert!(!app.worker_ready);
    assert_eq!(app.display_runtime_state(), RuntimeState::Starting);
}

fn preview_event(text: &str, recording_s: f64) -> WorkerEvent {
    WorkerEvent {
        event: "status".to_owned(),
        state: Some("preview".to_owned()),
        payload: serde_json::json!({
            "event": "status",
            "state": "preview",
            "text_preview": text,
            "recording_s": recording_s,
        }),
    }
}

#[test]
fn preview_status_captures_text_without_clearing_recording_stage() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;

    // Enter the recording stage (live spinner showing).
    app.update_worker_status(&status_event("recording"));
    assert_eq!(app.pipeline_stage, Some("recording"));
    assert_eq!(app.pipeline_preview, None);

    // A preview tick must keep the recording stage (NOT clear the spinner) and
    // capture the growing partial text.
    app.update_worker_status(&preview_event("hello there", 1.5));
    assert_eq!(
        app.pipeline_stage,
        Some("recording"),
        "preview must not clear the active recording stage"
    );
    assert_eq!(app.pipeline_preview.as_deref(), Some("hello there"));
    // Capture is still active and the worker stays ready.
    assert!(app.audio_capture_active);
    assert!(app.worker_ready);

    // A later preview replaces the text.
    app.update_worker_status(&preview_event("hello there friend", 3.0));
    assert_eq!(app.pipeline_preview.as_deref(), Some("hello there friend"));

    // Moving on to transcribing drops the stale preview text.
    app.update_worker_status(&status_event("transcribing"));
    assert_eq!(app.pipeline_stage, Some("transcribing"));
    assert_eq!(app.pipeline_preview, None);
}

#[test]
fn stop_runtime_clears_stale_pipeline_progress() {
    // Root-cause guard: stopping the worker mid-recording must clear the live
    // pipeline-progress state so the sidebar indicator and the progress card
    // can't stick on a stale "recording" stage after the worker is gone.
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    app.update_worker_status(&status_event("recording"));
    assert_eq!(app.pipeline_stage, Some("recording"));
    app.pipeline_preview = Some("partial text".to_owned());

    app.stop_runtime();

    assert_eq!(app.pipeline_stage, None);
    assert_eq!(app.pipeline_preview, None);
}

#[test]
fn push_to_talk_keys_render_as_friendly_chord() {
    assert_eq!(format_push_to_talk_keys("ctrl_r"), "Ctrl (right)");
    assert_eq!(
        format_push_to_talk_keys("shift_l+ctrl_l"),
        "Shift (left) + Ctrl (left)"
    );
    assert_eq!(format_push_to_talk_keys("alt"), "Alt");
    assert_eq!(format_push_to_talk_keys("space"), "Space");
    // Whitespace around chord separators is tolerated.
    assert_eq!(
        format_push_to_talk_keys(" ctrl_r + shift_r "),
        "Ctrl (right) + Shift (right)"
    );
    // Unknown tokens pass through capitalized so custom keys still read sensibly.
    assert_eq!(format_push_to_talk_keys("f12"), "F12");
    // Empty / blank input has no configured key.
    assert_eq!(format_push_to_talk_keys(""), "None");
    assert_eq!(format_push_to_talk_keys("  "), "None");
}

#[test]
fn badge_label_reflects_toggle_mode() {
    // Hold mode keeps the "Push-to-talk" prefix; toggle mode switches to the
    // "Toggle key" prefix while the chord rendering is unchanged.
    assert_eq!(
        push_to_talk_badge_label("ctrl_r", false, "en"),
        "Push-to-talk: Ctrl (right)"
    );
    assert_eq!(
        push_to_talk_badge_label("ctrl_r", true, "en"),
        "Toggle key: Ctrl (right)"
    );
    assert_eq!(
        push_to_talk_badge_label("shift_r+ctrl_r", true, "da"),
        "Skiftetast: Shift (right) + Ctrl (right)"
    );
}
