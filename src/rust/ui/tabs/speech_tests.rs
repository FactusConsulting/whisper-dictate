use crate::runtime::RuntimeState;
use crate::ui::{
    test_support::test_app, AppSettings, HotkeyCaptureState, HotkeyVerificationSession,
};

#[test]
fn applying_a_captured_shortcut_updates_settings_and_status() {
    let mut app = test_app(AppSettings::default());
    app.hotkey_capture = HotkeyCaptureState::Pending("ctrl_l+f9".to_owned());

    app.apply_captured_hotkey("ctrl_l+f9");

    assert_eq!(app.settings.key, "ctrl_l+f9");
    assert_eq!(app.hotkey_capture, HotkeyCaptureState::Idle);
    assert!(app.settings_status.contains("Shortcut set to ctrl_l+f9"));
}

#[test]
fn applying_an_unsupported_shortcut_cancels_capture_without_changing_settings() {
    let mut app = test_app(AppSettings::default());
    let original = app.settings.key.clone();
    app.hotkey_capture = HotkeyCaptureState::Pending("no-such-key".to_owned());

    app.apply_captured_hotkey("no-such-key");

    assert_eq!(app.settings.key, original);
    assert_eq!(app.hotkey_capture, HotkeyCaptureState::Idle);
    assert!(app.settings_status.contains("not supported"));
}

#[test]
fn capture_requires_a_stopped_runtime() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;

    app.start_hotkey_capture();

    assert_eq!(app.hotkey_capture, HotkeyCaptureState::Idle);
    assert!(app.settings_status.contains("Stop the runtime"));
}

#[test]
fn capture_cannot_overlap_an_active_guided_verification() {
    let mut app = test_app(AppSettings::default());
    let (session, _tx) = HotkeyVerificationSession::synthetic("pause", "test-stub");
    app.hotkey_verification_session = Some(session);

    app.start_hotkey_capture();

    assert_eq!(app.hotkey_capture, HotkeyCaptureState::Idle);
    assert!(app.hotkey_verification_session.is_some());
    assert!(app
        .settings_status
        .contains("Stop the guided shortcut test"));
}
