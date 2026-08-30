use crate::runtime::RuntimeState;
use crate::ui::{
    test_support::test_app, AppSettings, HotkeyCaptureState, HotkeyVerificationSession,
    NEMOTRON_HOSTED_STT_BASE_URL, NEMOTRON_IN_PROCESS_STT_BASE_URL, NEMOTRON_MULTI_STT_MODEL,
};

use super::speech::nemotron_local_model_editor_enabled;

#[test]
fn multilingual_profile_route_keeps_the_custom_gguf_editor_available() {
    let mut app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "nemotron".to_owned(),
        stt_base_url: NEMOTRON_HOSTED_STT_BASE_URL.to_owned(),
        stt_model: NEMOTRON_MULTI_STT_MODEL.to_owned(),
        ..Default::default()
    });

    app.route_selected_nemotron_profile();

    assert_eq!(app.settings.stt_base_url, NEMOTRON_IN_PROCESS_STT_BASE_URL);
    assert!(nemotron_local_model_editor_enabled(
        app.current_cloud_provider(),
        &app.settings.stt_base_url,
    ));

    app.settings.stt_model = r"C:\models\custom-nemotron.gguf".to_owned();
    assert!(nemotron_local_model_editor_enabled(
        app.current_cloud_provider(),
        &app.settings.stt_base_url,
    ));
}

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
