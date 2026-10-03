use super::*;

// Judge the hidden-smoke child by PROGRESS, not by total age. GitHub's shared
// Windows runners make renderer initialization intermittently slow (#892: the
// previous 30 s total budget failed on loaded runners while every phase kept
// advancing), but a healthy run keeps emitting `[hidden-smoke]` milestones
// however slowly. The watchdog therefore trips only when no milestone arrived
// within the stall limit, or when a progressing child still blows the total
// backstop; both bounds sit far outside every observed duration, so a trip
// names a genuine wedge together with the per-phase timing.
const STALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);
const TOTAL_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);

/// Watchdog decision for the hidden Windows event-loop smoke, extracted so the
/// bounds themselves stay unit-testable. `Some` means "kill the child and fail
/// with this reason": either no milestone arrived for `last_progress_age` (the
/// renderer or event loop wedged) or a progressing child exceeded the total
/// backstop (the drain loop terminates on its own, so progress without exit is
/// itself a wedge).
fn smoke_watchdog_violation(
    last_progress_age: std::time::Duration,
    total_elapsed: std::time::Duration,
) -> Option<String> {
    if last_progress_age >= STALL_LIMIT {
        return Some(format!(
            "hidden eframe child made no progress for {last_progress_age:?}"
        ));
    }
    if total_elapsed >= TOTAL_LIMIT {
        return Some(format!(
            "hidden eframe child kept progressing but did not finish within {TOTAL_LIMIT:?}"
        ));
    }
    None
}

#[test]
fn smoke_watchdog_ignores_slow_but_progressing_children() {
    use std::time::Duration;
    // Every healthy duration observed in CI fits far inside both bounds.
    assert_eq!(
        smoke_watchdog_violation(Duration::from_secs(0), Duration::from_secs(599)),
        None
    );
    assert_eq!(
        smoke_watchdog_violation(Duration::from_secs(119), Duration::from_secs(599)),
        None
    );
}

#[test]
fn smoke_watchdog_fails_stalled_children_with_the_stall_reason() {
    use std::time::Duration;
    let verdict = smoke_watchdog_violation(Duration::from_secs(121), Duration::from_secs(121))
        .expect("a stalled child must trip the watchdog");
    assert!(verdict.contains("no progress"), "{verdict}");
    assert!(!verdict.contains("did not finish within"), "{verdict}");
}

#[test]
fn smoke_watchdog_backstops_progressing_children_that_never_finish() {
    use std::time::Duration;
    let verdict = smoke_watchdog_violation(Duration::from_secs(0), Duration::from_secs(600))
        .expect("the total backstop must trip");
    assert!(verdict.contains("did not finish within"), "{verdict}");
}

#[test]
fn injection_stage_uses_mouse_passthrough() {
    assert!(injection_viewport_mouse_passthrough(Some("injecting")));
    assert!(!injection_viewport_mouse_passthrough(Some("recording")));
    assert!(!injection_viewport_mouse_passthrough(None));
}

#[cfg(windows)]
#[test]
fn hidden_windows_event_loop_drains_runtime_and_tray_without_ui() {
    const CHILD_ENV: &str = "VOICEPI_TEST_HIDDEN_EFRAME_CHILD";
    const TEST_NAME: &str =
        "ui::app_tests::view::hidden_windows_event_loop_drains_runtime_and_tray_without_ui";

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
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn hidden eframe smoke child");
    // Read progress independently: waiting for a line on the parent thread
    // would defeat the watchdog if native renderer/event-loop startup wedges.
    let stdout = child.stdout.take().expect("hidden smoke stdout");
    let (progress_tx, progress_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            eprintln!("{line}");
            if line.starts_with("[hidden-smoke] ") {
                let _ = progress_tx.send(line);
            }
        }
    });
    let started = std::time::Instant::now();
    let mut last_progress_at = started;
    let mut last_progress = "child-spawned".to_owned();
    let mut phases = Vec::new();
    let mut phase_times: Vec<(String, std::time::Duration)> = Vec::new();
    loop {
        let mut progress_seen = false;
        for progress in progress_rx.try_iter() {
            phases.push(progress.clone());
            phase_times.push((progress.clone(), started.elapsed()));
            last_progress = progress;
            progress_seen = true;
        }
        if progress_seen {
            last_progress_at = std::time::Instant::now();
        }
        if let Some(status) = child.try_wait().expect("poll hidden eframe child") {
            reader.join().expect("join hidden smoke stdout reader");
            for progress in progress_rx.try_iter() {
                phases.push(progress.clone());
                phase_times.push((progress.clone(), started.elapsed()));
                last_progress = progress;
            }
            assert!(
                status.success(),
                "hidden eframe child failed: {status}; last progress: {last_progress}"
            );
            for phase in [
                "event-loop-configuring",
                "window-configuring",
                "adapter-selected",
                "renderer-ready",
            ] {
                assert!(
                    phases
                        .iter()
                        .any(|line| line == &format!("[hidden-smoke] phase={phase}")),
                    "hidden eframe child missed startup phase {phase}"
                );
            }
            break;
        }
        if let Some(verdict) =
            smoke_watchdog_violation(last_progress_at.elapsed(), started.elapsed())
        {
            let _ = child.kill();
            let _ = child.wait();
            reader.join().expect("join hidden smoke stdout reader");
            for progress in progress_rx.try_iter() {
                last_progress = progress;
            }
            panic!("{verdict}; last progress: {last_progress}; phases so far: {phase_times:?}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn run_hidden_windows_event_loop_child() {
    use crate::ui::TrayState;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    fn progress(phase: &str) {
        use std::io::Write;
        println!("[hidden-smoke] phase={phase}");
        std::io::stdout()
            .flush()
            .expect("flush hidden smoke progress");
    }
    progress("child-started");

    fn request_hidden_repaint(ctx: &egui::Context) {
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            repaint.request_repaint();
        });
    }

    struct HiddenEventLoopApp {
        inner: crate::ui::WhisperDictateApp,
        tx: std::sync::mpsc::Sender<RuntimeEvent>,
        minimize_requested: bool,
        hidden_phase: u8,
        close_requested: bool,
        recording_tray_seen: bool,
        processed: Arc<AtomicBool>,
        ui_before_processed: Arc<AtomicBool>,
    }

    impl eframe::App for HiddenEventLoopApp {
        fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            let just_requested_minimize = !self.minimize_requested;
            if just_requested_minimize {
                progress("first-logic");
            }
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
                progress("minimized");
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
                progress("recording-drained");
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
                if !self.processed.load(Ordering::SeqCst) {
                    progress("error-drained");
                }
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
                if !self.close_requested {
                    progress("close-requested");
                    self.close_requested = true;
                }
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
    let mut wgpu_options = eframe::WgpuConfiguration::default();
    let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup else {
        panic!("default WGPU setup must create a device");
    };
    let default_device_descriptor = Arc::clone(&setup.device_descriptor);
    setup.device_descriptor = Arc::new(move |adapter| {
        progress("adapter-selected");
        default_device_descriptor(adapter)
    });
    let options = eframe::NativeOptions {
        renderer,
        event_loop_builder: Some(Box::new(|builder| {
            progress("event-loop-configuring");
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        window_builder: Some(Box::new(|builder| {
            progress("window-configuring");
            builder
        })),
        wgpu_options,
        viewport: egui::ViewportBuilder::default().with_inner_size([320.0, 200.0]),
        ..Default::default()
    };

    progress("renderer-initializing");
    eframe::run_native(
        "whisper-dictate hidden-event-loop smoke",
        options,
        Box::new(move |_cc| {
            // eframe invokes AppCreator only after window and WGPU painter setup.
            progress("renderer-ready");
            let mut app = test_app(AppSettings::default());
            app.audio_devices_loaded = true;
            app.settings.update_check = false;
            app.tray.disable();
            app.supervisor.set_running_for_tests();
            let tx = app.supervisor.event_sender_for_tests();
            progress("app-created");
            Ok(Box::new(HiddenEventLoopApp {
                inner: app,
                tx,
                minimize_requested: false,
                hidden_phase: 0,
                close_requested: false,
                recording_tray_seen: false,
                processed: processed_for_app,
                ui_before_processed: ui_before_processed_for_app,
            }))
        }),
    )
    .expect("run hidden Windows eframe smoke");
    progress("event-loop-returned");

    assert!(
        processed.load(Ordering::SeqCst),
        "hidden event loop did not drain runtime events and sync tray state"
    );
    assert!(
        !ui_before_processed.load(Ordering::SeqCst),
        "viewport ran UI before hidden runtime/tray processing completed"
    );
}
