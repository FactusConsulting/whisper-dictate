use super::*;

#[test]
fn runtime_started_event_records_the_actual_installed_hotkey() {
    let mut app = test_app(AppSettings::default());
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app.supervisor.send_event_for_tests(RuntimeEvent::Started {
        command: "native-rust".to_owned(),
        hotkey_driver: "win_registerhotkey".to_owned(),
        hotkey_chord: "pause".to_owned(),
    });

    let ctx = egui::Context::default();
    let mut frame = eframe::Frame::_new_kittest();
    eframe::App::logic(&mut app, &ctx, &mut frame);

    assert_eq!(
        app.installed_hotkey,
        Some(InstalledHotkeyStatus {
            chord: "pause".to_owned(),
            driver: "win_registerhotkey".to_owned(),
        })
    );
}

#[test]
fn recoverable_runtime_error_preserves_the_installed_hotkey_status() {
    let mut app = test_app(AppSettings::default());
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app.supervisor.send_event_for_tests(RuntimeEvent::Started {
        command: "native-rust".to_owned(),
        hotkey_driver: "win_registerhotkey".to_owned(),
        hotkey_chord: "pause".to_owned(),
    });

    let ctx = egui::Context::default();
    let mut frame = eframe::Frame::_new_kittest();
    eframe::App::logic(&mut app, &ctx, &mut frame);
    app.supervisor
        .send_event_for_tests(RuntimeEvent::Error("transcription failed".to_owned()));
    eframe::App::logic(&mut app, &ctx, &mut frame);

    assert_eq!(
        app.installed_hotkey,
        Some(InstalledHotkeyStatus {
            chord: "pause".to_owned(),
            driver: "win_registerhotkey".to_owned(),
        })
    );
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("transcription failed")
    );
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
