use crate::runtime::RuntimeState;
use crate::ui::{test_support::test_app, AppSettings};

use super::hotkey_verify::guided_hotkey_verification_available;

#[test]
fn guided_test_is_disabled_when_focus_attribution_is_unavailable() {
    assert!(!guided_hotkey_verification_available("pause", false));
    #[cfg(feature = "rust-hotkeys")]
    assert!(guided_hotkey_verification_available("pause", true));
}

#[test]
fn guided_test_rejects_invalid_syntax_before_listener_install() {
    let mut app = test_app(AppSettings::default());
    app.settings.key = "not-a-hotkey".to_owned();

    app.start_hotkey_verification(&eframe::egui::Context::default());

    assert!(app.hotkey_verification_session.is_none());
    assert!(app.settings_status.contains("invalid shortcut"));
}

#[test]
fn guided_test_cannot_overlap_normal_dictation() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;

    app.start_hotkey_verification(&eframe::egui::Context::default());

    assert!(app.hotkey_verification_session.is_none());
    assert!(app.settings_status.contains("Stop dictation"));
}
