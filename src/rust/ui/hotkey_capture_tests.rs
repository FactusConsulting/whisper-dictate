use super::{capture_token_for_egui_key, HotkeyCaptureState};
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
