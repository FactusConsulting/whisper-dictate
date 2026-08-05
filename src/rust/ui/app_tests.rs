use super::{test_support::test_app, AppSettings, HotkeyCaptureState};
use eframe::egui;

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
