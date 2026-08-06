use super::{test_support::test_app, AppSettings, HotkeyCaptureState, WorkerEvent};
use eframe::egui;
use serde_json::json;

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
            "text": "  hello from the floating surface  ",
            "profile": "terminal",
            "target_title": "PowerShell",
            "target_process": "pwsh.exe"
        }),
    });

    assert_eq!(
        app.last_transcript.as_deref(),
        Some("hello from the floating surface")
    );
    assert_eq!(app.active_profile.as_deref(), Some("terminal"));
    assert_eq!(app.last_target_title, "PowerShell");
    assert_eq!(app.last_target_process, "pwsh.exe");
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
