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

    app.update_worker_status(&preview_event("hello there friend", 3.0));
    assert_eq!(app.pipeline_preview.as_deref(), Some("hello there friend"));

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

fn device_unusable_event(device: &str, error: &str) -> WorkerEvent {
    WorkerEvent {
        event: "status".to_owned(),
        state: Some("error".to_owned()),
        payload: serde_json::json!({
            "event": "status",
            "state": "error",
            "reason": "device_unusable",
            "audio_device": device,
            "error": error,
        }),
    }
}

fn working_device_event(device: &str) -> WorkerEvent {
    WorkerEvent {
        event: "status".to_owned(),
        state: Some("recording".to_owned()),
        payload: serde_json::json!({
            "event": "status",
            "state": "recording",
            "audio_device": device,
        }),
    }
}

fn recovered_device_event(device: &str) -> WorkerEvent {
    WorkerEvent {
        event: "status".to_owned(),
        state: Some("audio-recovered".to_owned()),
        payload: serde_json::json!({
            "event": "status",
            "state": "audio-recovered",
            "audio_device": device,
        }),
    }
}

#[test]
fn device_unusable_status_sets_error_banner_and_clears_on_working_device() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    assert!(app.device_error.is_none());

    // The worker reports the picked mic can't be opened on any backend.
    app.update_worker_status(&device_unusable_event(
        "Microphone (Yeti)",
        "Microphone 'Microphone (Yeti)' could not be opened on any audio backend \
         — select a different microphone in Settings.",
    ));
    let banner = app
        .device_error
        .clone()
        .expect("device_error should be set");
    assert!(
        banner.contains("could not be opened on any audio backend"),
        "{banner}"
    );
    // The bad device is recorded as the active device (so the UI shows it too).
    assert_eq!(app.active_audio_device, "Microphone (Yeti)");

    // A subsequent recording on a working mic clears the banner.
    app.update_worker_status(&working_device_event("Headset Mic"));
    assert!(
        app.device_error.is_none(),
        "a working device must clear the unusable banner"
    );
    assert_eq!(app.active_audio_device, "Headset Mic");
}

#[test]
fn audio_recovery_clears_the_device_banner_and_stale_runtime_error() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    app.update_worker_status(&device_unusable_event(
        "Disconnected USB microphone",
        "System default microphone is unavailable; retrying in the background",
    ));
    assert!(app.device_error.is_some());
    assert!(app.last_runtime_error.is_some());

    app.update_worker_status(&recovered_device_event("System default"));

    assert!(app.device_error.is_none());
    assert!(app.last_runtime_error.is_none());
    assert!(!app.last_injection_failed);
    assert_eq!(app.active_audio_device, "System default");
}

#[test]
fn audio_recovery_preserves_the_active_utterance_stage() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    app.update_worker_status(&status_event("recording"));
    assert_eq!(app.pipeline_stage, Some("recording"));
    assert_eq!(app.last_worker_status_state, "recording");

    app.update_worker_status(&recovered_device_event("System default"));

    assert_eq!(app.pipeline_stage, Some("recording"));
    assert_eq!(app.last_worker_status_state, "recording");
    assert_eq!(app.active_audio_device, "System default");
}

#[test]
fn ordinary_error_without_device_unusable_reason_does_not_set_banner() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    // A generic error (e.g. a failed model load) must NOT raise the mic banner.
    app.update_worker_status(&status_event("failed"));
    assert!(app.device_error.is_none());
}

// -----------------------------------------------------------------------
// Push-to-talk ownership refusal banner (2026-07-29 interleaved-injection
// regression). The GUI is a windows-subsystem binary, so the stderr line
// the CLI operator reads goes nowhere -- if the banner does not appear,
// the tray app simply stops answering the hotkey with no explanation,
// which is its own bug.
// -----------------------------------------------------------------------

/// Serialises against the other tests that touch the process-wide refusal
/// slot. Uses the same lock the `ptt_lock` companion tests take, so the
/// two suites cannot interleave on it.
fn ptt_slot_guard() -> std::sync::MutexGuard<'static, ()> {
    crate::hotkey::ptt_lock::report::TEST_SLOT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn refusal_conflict() -> crate::hotkey::ptt_lock::PttConflict {
    crate::hotkey::ptt_lock::PttConflict {
        chord: "f9".to_owned(),
        holder: Some(crate::hotkey::ptt_lock::HolderRecord::new(
            12345,
            "whisper-dictate-gui",
            "none",
            "win_registerhotkey",
            "f9",
        )),
        lock_path: "/tmp/whisper-dictate-ptt-alice.lock".to_owned(),
    }
}

#[test]
fn a_refused_push_to_talk_registration_raises_the_hotkey_banner() {
    let _guard = ptt_slot_guard();
    crate::hotkey::ptt_lock::report::clear();

    let mut app = test_app(AppSettings::default());
    app.refresh_hotkey_conflict();
    assert!(
        app.hotkey_conflict.is_none(),
        "a lone process must not show a conflict banner"
    );

    crate::hotkey::ptt_lock::report::record(refusal_conflict());
    app.refresh_hotkey_conflict();
    let banner = app
        .hotkey_conflict
        .clone()
        .expect("a refused registration must raise the banner");
    // The banner is the ONLY place a GUI user learns why the hotkey stopped
    // working, so it has to carry the whole story: which pid to quit, and
    // what the refusal prevented.
    assert!(banner.contains("pid 12345"), "{banner}");
    assert!(banner.contains("f9"), "{banner}");
    assert!(banner.contains("interleaving"), "{banner}");

    crate::hotkey::ptt_lock::report::clear();
}

#[test]
fn the_hotkey_banner_clears_once_ownership_is_regained() {
    // The user quits the other process and restarts; a successful install
    // retracts the published refusal, and the banner must follow it down
    // rather than sticking until the next app restart.
    let _guard = ptt_slot_guard();
    crate::hotkey::ptt_lock::report::record(refusal_conflict());

    let mut app = test_app(AppSettings::default());
    app.refresh_hotkey_conflict();
    assert!(app.hotkey_conflict.is_some());

    crate::hotkey::ptt_lock::report::clear();
    app.refresh_hotkey_conflict();
    assert!(
        app.hotkey_conflict.is_none(),
        "regaining push-to-talk ownership must take the banner down"
    );
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
