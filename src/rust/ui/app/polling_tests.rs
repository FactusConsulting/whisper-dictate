use super::*;

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
fn hidden_logic_drains_worker_events_without_a_ui_pass() {
    let mut app = test_app(AppSettings::default());
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app.supervisor
        .send_event_for_tests(RuntimeEvent::Worker(WorkerEvent {
            event: "utterance".to_owned(),
            state: None,
            payload: json!({"text": "processed while hidden"}),
        }));

    assert!(app.last_transcript.is_none());
    let ctx = egui::Context::default();
    let mut frame = eframe::Frame::_new_kittest();
    eframe::App::logic(&mut app, &ctx, &mut frame);

    assert_eq!(
        app.last_transcript.as_deref(),
        Some("processed while hidden")
    );
}

#[test]
fn leaving_the_speech_tab_cancels_capture() {
    let mut app = test_app(AppSettings::default());
    app.hotkey_capture.start();
    app.selected_tab = crate::ui::Tab::Log;

    app.cancel_hotkey_capture_if_hidden();

    assert_eq!(app.hotkey_capture, HotkeyCaptureState::Idle);
    assert!(app.settings_status.contains("Speech tab"));
}

#[test]
fn leaving_the_speech_tab_stops_the_guided_hotkey_process() {
    let mut app = test_app(AppSettings::default());
    let (session, _tx) = HotkeyVerificationSession::synthetic("pause", "test-stub");
    app.hotkey_verification_session = Some(session);
    app.selected_tab = crate::ui::Tab::Log;

    app.cancel_hotkey_verification_if_controls_hidden();

    assert!(app.hotkey_verification_session.is_none());
    assert!(app.hotkey_verification.is_some());
    assert!(app.settings_status.contains("controls were hidden"));
}

#[test]
fn entering_compact_mode_stops_the_guided_hotkey_process() {
    let mut app = test_app(AppSettings::default());
    let (session, _tx) = HotkeyVerificationSession::synthetic("pause", "test-stub");
    app.hotkey_verification_session = Some(session);
    app.selected_tab = crate::ui::Tab::Speech;
    app.compact_mode = true;

    app.cancel_hotkey_verification_if_controls_hidden();

    assert!(app.hotkey_verification_session.is_none());
    assert!(app.hotkey_verification.is_some());
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
