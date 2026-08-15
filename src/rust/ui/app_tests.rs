use super::app::injection_viewport_mouse_passthrough;
use super::tasks::REINJECT_LAST_LABEL;
use super::{
    test_support::test_app, AppSettings, HotkeyCaptureState, HotkeyVerificationSession,
    InstalledHotkeyStatus, WorkerEvent,
};
use crate::runtime::RuntimeEvent;
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
fn injection_stage_uses_mouse_passthrough() {
    assert!(injection_viewport_mouse_passthrough(Some("injecting")));
    assert!(!injection_viewport_mouse_passthrough(Some("recording")));
    assert!(!injection_viewport_mouse_passthrough(None));
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

#[cfg(windows)]
#[test]
fn hidden_windows_event_loop_drains_runtime_and_tray_without_ui() {
    const CHILD_ENV: &str = "VOICEPI_TEST_HIDDEN_EFRAME_CHILD";
    const TEST_NAME: &str =
        "ui::app_tests::hidden_windows_event_loop_drains_runtime_and_tray_without_ui";

    if std::env::var_os(CHILD_ENV).is_some() {
        run_hidden_windows_event_loop_child();
        return;
    }

    // A native event loop is process-global on some winit platforms. Isolate
    // it from the parallel unit-test process and enforce a bounded failure if
    // hidden repaint dispatch ever wedges.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_ENV, "1")
        .spawn()
        .expect("spawn hidden eframe smoke child");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().expect("poll hidden eframe child") {
            assert!(status.success(), "hidden eframe child failed: {status}");
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("hidden eframe child did not finish within 30 seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn run_hidden_windows_event_loop_child() {
    use super::TrayState;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    fn request_hidden_repaint(ctx: &egui::Context) {
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            repaint.request_repaint();
        });
    }

    struct HiddenEventLoopApp {
        inner: super::WhisperDictateApp,
        tx: std::sync::mpsc::Sender<RuntimeEvent>,
        minimize_requested: bool,
        hidden_phase: u8,
        recording_tray_seen: bool,
        processed: Arc<AtomicBool>,
        ui_before_processed: Arc<AtomicBool>,
    }

    impl eframe::App for HiddenEventLoopApp {
        fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            let just_requested_minimize = !self.minimize_requested;
            let minimized = frame.winit_window().is_some_and(|window| {
                if just_requested_minimize {
                    window.set_minimized(true);
                    window.request_redraw();
                    self.minimize_requested = true;
                }
                window.is_minimized() == Some(true)
            });
            eframe::App::logic(&mut self.inner, ctx, frame);

            // Queue after the first confirmed minimized logic pass has polled,
            // forcing a second event-loop dispatch to drain these events.
            if minimized && !just_requested_minimize && self.hidden_phase == 0 {
                self.tx
                    .send(RuntimeEvent::Worker(WorkerEvent {
                        event: "status".to_owned(),
                        state: Some("recording".to_owned()),
                        payload: json!({"audio_device": "hidden-smoke"}),
                    }))
                    .expect("queue hidden recording event");
                self.hidden_phase = 1;
                request_hidden_repaint(ctx);
                return;
            }

            if self.hidden_phase == 1
                && self.inner.last_logged_tray_state == Some(TrayState::Recording)
            {
                self.recording_tray_seen = true;
                self.tx
                    .send(RuntimeEvent::Error("hidden event-loop error".to_owned()))
                    .expect("queue hidden runtime error");
                self.hidden_phase = 2;
                request_hidden_repaint(ctx);
                return;
            }

            let error_drained =
                self.inner.last_runtime_error.as_deref() == Some("hidden event-loop error");
            let error_tray_updated = self.inner.last_logged_tray_state == Some(TrayState::Ready);
            if self.hidden_phase == 2
                && self.recording_tray_seen
                && error_drained
                && error_tray_updated
            {
                self.processed.store(true, Ordering::SeqCst);
                if let Some(window) = frame.winit_window() {
                    window.set_minimized(false);
                    window.request_redraw();
                }
                ctx.request_repaint();
            }
        }

        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            if self.processed.load(Ordering::SeqCst) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else if self.hidden_phase > 0 {
                self.ui_before_processed.store(true, Ordering::SeqCst);
            }
            eframe::App::ui(&mut self.inner, ui, frame);
        }
    }

    let processed = Arc::new(AtomicBool::new(false));
    let ui_before_processed = Arc::new(AtomicBool::new(false));
    let processed_for_app = Arc::clone(&processed);
    let ui_before_processed_for_app = Arc::clone(&ui_before_processed);
    // Windows test builds enable eframe/wgpu as a dev-only feature so this
    // native event-loop smoke works on headless CI via the DX12 WARP adapter.
    // The shipped renderer feature selection remains unchanged.
    let renderer = eframe::Renderer::Wgpu;
    let options = eframe::NativeOptions {
        renderer,
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        viewport: egui::ViewportBuilder::default().with_inner_size([320.0, 200.0]),
        ..Default::default()
    };

    eframe::run_native(
        "whisper-dictate hidden-event-loop smoke",
        options,
        Box::new(move |_cc| {
            let mut app = test_app(AppSettings::default());
            app.audio_devices_loaded = true;
            app.settings.update_check = false;
            app.tray.disable();
            app.supervisor.set_running_for_tests();
            let tx = app.supervisor.event_sender_for_tests();
            Ok(Box::new(HiddenEventLoopApp {
                inner: app,
                tx,
                minimize_requested: false,
                hidden_phase: 0,
                recording_tray_seen: false,
                processed: processed_for_app,
                ui_before_processed: ui_before_processed_for_app,
            }))
        }),
    )
    .expect("run hidden Windows eframe smoke");

    assert!(
        processed.load(Ordering::SeqCst),
        "hidden event loop did not drain runtime events and sync tray state"
    );
    assert!(
        !ui_before_processed.load(Ordering::SeqCst),
        "viewport ran UI before hidden runtime/tray processing completed"
    );
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
fn leaving_the_speech_tab_stops_the_guided_hotkey_process() {
    let mut app = test_app(AppSettings::default());
    let (session, _tx) = HotkeyVerificationSession::synthetic("pause", "test-stub");
    app.hotkey_verification_session = Some(session);
    app.selected_tab = super::Tab::Log;

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
    app.selected_tab = super::Tab::Speech;
    app.compact_mode = true;

    app.cancel_hotkey_verification_if_controls_hidden();

    assert!(app.hotkey_verification_session.is_none());
    assert!(app.hotkey_verification.is_some());
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
