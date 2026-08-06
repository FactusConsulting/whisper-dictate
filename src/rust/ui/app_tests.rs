use super::tasks::REINJECT_LAST_LABEL;
use super::{test_support::test_app, AppSettings, HotkeyCaptureState, WorkerEvent};
use eframe::egui;
use serde_json::json;
use std::sync::mpsc;

fn key_event(key: egui::Key, pressed: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }
}

#[test]
fn capture_events_use_physical_modifier_identity() {
    let mut app = test_app(AppSettings::default());
    app.hotkey_capture.start();
    app.poll_hotkey_capture_events(vec![
        key_event(egui::Key::ControlLeft, true),
        key_event(egui::Key::F9, true),
        key_event(egui::Key::F9, false),
        key_event(egui::Key::ControlLeft, false),
    ]);

    assert_eq!(
        app.hotkey_capture,
        HotkeyCaptureState::Pending("ctrl_l+f9".to_owned())
    );
}

#[test]
fn focus_loss_cancels_an_in_progress_capture() {
    let mut app = test_app(AppSettings::default());
    app.hotkey_capture.start();

    app.poll_hotkey_capture_events(vec![egui::Event::WindowFocused(false)]);

    assert_eq!(app.hotkey_capture, HotkeyCaptureState::Idle);
    assert!(app.settings_status.contains("lost focus"));
}

#[test]
fn leaving_the_speech_tab_cancels_capture() {
    let mut app = test_app(AppSettings::default());
    app.hotkey_capture.start();
    app.selected_tab = super::Tab::Log;

    app.cancel_hotkey_capture_if_hidden();

    assert_eq!(app.hotkey_capture, HotkeyCaptureState::Idle);
    assert!(app.settings_status.contains("Speech tab"));
}

#[test]
fn utterance_event_populates_status_surface_preview_and_target() {
    let mut app = test_app(AppSettings::default());
    app.handle_worker_event(&WorkerEvent {
        event: "utterance".to_owned(),
        state: None,
        payload: json!({
            "text": "\nhello from the floating surface\n",
            "profile": "terminal",
            "target_title": "PowerShell",
            "target_process": "pwsh.exe",
            "target_id": "42",
            "inject_mode": "paste"
        }),
    });

    assert_eq!(
        app.last_transcript.as_deref(),
        Some("\nhello from the floating surface\n")
    );
    assert_eq!(app.active_profile.as_deref(), Some("terminal"));
    assert_eq!(app.last_target_title, "PowerShell");
    assert_eq!(app.last_target_process, "pwsh.exe");
    assert_eq!(app.last_target_id, "42");
    assert_eq!(app.last_inject_mode.as_deref(), Some("paste"));
}

#[test]
fn profile_status_updates_target_before_recording() {
    let mut app = test_app(AppSettings::default());
    app.update_worker_status(&WorkerEvent {
        event: "status".to_owned(),
        state: Some("profile".to_owned()),
        payload: json!({
            "active_profile": "terminal",
            "target_title": "PowerShell",
            "target_process": "pwsh.exe",
            "target_id": "42"
        }),
    });

    assert_eq!(app.active_profile.as_deref(), Some("terminal"));
    assert_eq!(app.active_target_title, "PowerShell");
    assert_eq!(app.active_target_process, "pwsh.exe");
    assert_eq!(app.active_target_id, "42");
    assert!(app.last_target_title.is_empty());
}

#[test]
fn worker_error_is_visible_until_ready() {
    let mut app = test_app(AppSettings::default());
    app.update_worker_status(&WorkerEvent {
        event: "status".to_owned(),
        state: Some("error".to_owned()),
        payload: json!({"error": "microphone unavailable"}),
    });
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("microphone unavailable")
    );

    app.update_worker_status(&WorkerEvent {
        event: "status".to_owned(),
        state: Some("ready".to_owned()),
        payload: json!({}),
    });
    assert!(app.last_runtime_error.is_none());
}

#[test]
fn no_text_error_remains_visible_through_ready() {
    let mut app = test_app(AppSettings::default());
    app.update_worker_status(&WorkerEvent {
        event: "status".to_owned(),
        state: Some("no_text".to_owned()),
        payload: json!({"error": "transcription request failed"}),
    });
    app.update_worker_status(&WorkerEvent {
        event: "status".to_owned(),
        state: Some("ready".to_owned()),
        payload: json!({}),
    });

    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("transcription request failed")
    );
}

#[test]
fn injection_error_survives_the_following_ready_event() {
    let mut app = test_app(AppSettings::default());
    app.handle_worker_event(&WorkerEvent {
        event: "utterance".to_owned(),
        state: None,
        payload: json!({
            "text": "hello",
            "inject_error": "target rejected input"
        }),
    });
    app.update_worker_status(&WorkerEvent {
        event: "status".to_owned(),
        state: Some("ready".to_owned()),
        payload: json!({}),
    });

    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("target rejected input")
    );
    assert!(app.last_injection_failed);
}

#[test]
fn empty_profile_status_clears_the_previous_profile() {
    let mut app = test_app(AppSettings::default());
    app.update_worker_status(&WorkerEvent {
        event: "status".to_owned(),
        state: Some("profile".to_owned()),
        payload: json!({"active_profile": "terminal"}),
    });
    assert_eq!(app.active_profile.as_deref(), Some("terminal"));

    app.update_worker_status(&WorkerEvent {
        event: "status".to_owned(),
        state: Some("profile".to_owned()),
        payload: json!({"active_profile": ""}),
    });

    assert!(app.active_profile.is_none());
}

#[test]
fn start_blocked_by_a_transcript_action_advances_error_revision() {
    let mut app = test_app(AppSettings::default());
    let (_tx, rx) = mpsc::channel();
    app.background_task = Some(rx);
    app.background_task_label = Some(REINJECT_LAST_LABEL);
    let revision = app.runtime_error_revision;

    app.start_runtime();

    assert_eq!(app.runtime_error_revision, revision.wrapping_add(1));
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("Cannot start the runtime while a transcript action is running.")
    );
}

#[test]
fn lifecycle_gate_ignores_non_transcript_background_tasks() {
    let mut app = test_app(AppSettings::default());
    let (_tx, rx) = mpsc::channel();
    app.background_task = Some(rx);
    app.background_task_label = Some("doctor");

    assert!(!app.transcript_action_running());

    app.background_task_label = Some(REINJECT_LAST_LABEL);
    assert!(app.transcript_action_running());
}

#[test]
fn process_exit_replaces_stale_worker_error_but_preserves_runtime_diagnosis() {
    let mut app = test_app(AppSettings::default());
    app.last_runtime_error = Some("transcription request failed".to_owned());
    app.handle_runtime_exit(Some(17));
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("runtime exited with code 17")
    );

    app.last_runtime_error = Some("native runtime start failed at hotkey-install".to_owned());
    app.last_runtime_error_from_runtime = true;
    app.handle_runtime_exit(Some(17));
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("native runtime start failed at hotkey-install")
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_physical_modifier_events_follow_the_capture_path() {
    let mut app = test_app(AppSettings::default());
    app.hotkey_capture.start();
    app.poll_hotkey_capture_events(vec![
        key_event(egui::Key::ControlRight, true),
        key_event(egui::Key::F9, true),
        key_event(egui::Key::F9, false),
        key_event(egui::Key::ControlRight, false),
    ]);

    assert_eq!(
        app.hotkey_capture,
        HotkeyCaptureState::Pending("ctrl_r+f9".to_owned())
    );
}
