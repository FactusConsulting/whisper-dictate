use super::{capture_token_for_egui_key, HotkeyCaptureState};
use super::{test_support::test_app, AppSettings};
use eframe::egui;

#[test]
fn capture_keeps_left_and_right_modifiers_distinct() {
    let mut capture = HotkeyCaptureState::default();
    capture.start();
    capture.observe("ctrl_r", true);
    capture.observe("f9", true);
    capture.observe("f9", false);
    assert_eq!(
        capture.observe("ctrl_r", false),
        Some("ctrl_r+f9".to_owned())
    );
}

#[test]
fn capture_exposes_only_installable_special_keys() {
    assert_eq!(capture_token_for_egui_key(egui::Key::F12), Some("f12"));
    assert_eq!(capture_token_for_egui_key(egui::Key::Backspace), None);
    assert_eq!(capture_token_for_egui_key(egui::Key::F13), None);
}

#[test]
fn applying_capture_updates_settings_and_marks_them_dirty() {
    let mut app = test_app(AppSettings::default());
    app.hotkey_capture = HotkeyCaptureState::Pending("ctrl_r+f9".to_owned());
    app.apply_captured_hotkey("ctrl_r+f9");
    assert_eq!(app.settings.key, "ctrl_r+f9");
    assert_ne!(app.settings.key, app.saved_settings.key);
    assert_eq!(app.hotkey_capture, HotkeyCaptureState::Idle);
    assert!(app.settings_status.contains("Save settings"));
}

#[test]
fn applying_invalid_capture_discards_pending_value() {
    let mut app = test_app(AppSettings::default());
    let original = app.settings.key.clone();
    app.hotkey_capture = HotkeyCaptureState::Pending("not-a-hotkey".to_owned());
    app.apply_captured_hotkey("not-a-hotkey");
    assert_eq!(app.settings.key, original);
    assert_eq!(app.hotkey_capture, HotkeyCaptureState::Idle);
    assert!(app.settings_status.contains("not supported"));
}
