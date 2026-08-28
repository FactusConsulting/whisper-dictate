//! Integration coverage for native UI background-task wiring.

use super::*;
use crate::platform::window_enumeration::VisibleWindow;
use crate::ui::test_support::test_app;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn fixed_windows() -> Result<Vec<VisibleWindow>, String> {
    Ok(vec![
        VisibleWindow {
            title: "Notes - draft".to_owned(),
            process: "notepad.exe".to_owned(),
        },
        VisibleWindow {
            title: "Browser".to_owned(),
            process: "browser.exe".to_owned(),
        },
    ])
}

#[test]
fn run_list_windows_populates_profiles_options_after_background_completion() {
    let mut app = test_app(AppSettings::default());
    *TEST_WINDOW_ENUMERATOR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fixed_windows);

    app.run_list_windows();

    let deadline = Instant::now() + Duration::from_secs(2);
    while app.background_task.is_some() && Instant::now() < deadline {
        app.poll_background_task();
        std::thread::sleep(Duration::from_millis(1));
    }
    *TEST_WINDOW_ENUMERATOR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;

    assert!(
        app.background_task.is_none(),
        "background task did not finish"
    );
    assert_eq!(
        app.window_options,
        vec![
            ("Notes - draft".to_owned(), "notepad.exe".to_owned()),
            ("Browser".to_owned(), "browser.exe".to_owned()),
        ]
    );
    assert!(
        app.runtime_log
            .contains("[ui] window list refreshed: 2 window(s)"),
        "completion was not dispatched to the window-list handler: {}",
        app.runtime_log
    );
}

#[test]
fn reinject_last_reports_when_no_transcript_exists() {
    let mut app = test_app(AppSettings::default());
    app.run_reinject_last(REINJECT_LAST_LABEL);

    assert!(app.background_task.is_none());
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("No transcript is available yet.")
    );
    assert!(app.runtime_log.contains("no transcript available"));
}

#[test]
fn reinject_is_blocked_while_runtime_is_active() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    app.last_transcript = Some("hello".to_owned());

    app.run_reinject_last(REINJECT_LAST_LABEL);

    assert!(app.background_task.is_none());
    assert!(app
        .runtime_log
        .contains("stop the runtime before reinjecting"));
}

#[test]
fn reinject_uses_the_effective_configured_xkb_layout() {
    let settings = AppSettings {
        xkb_layout: " no ".to_owned(),
        ..Default::default()
    };
    assert_eq!(super::effective_reinject_xkb_layout(&settings), "no");
}

#[test]
fn reinjection_keeps_the_applied_runtime_layout_after_unsaved_edits() {
    let mut app = test_app(AppSettings::default());
    app.settings.xkb_layout = "no".to_owned();
    app.applied_settings.xkb_layout = "dk".to_owned();

    assert_eq!(
        super::effective_reinject_xkb_layout(&app.applied_settings),
        "dk"
    );
}

#[test]
fn reinject_failure_preserves_a_newer_runtime_error() {
    let mut app = test_app(AppSettings::default());
    let (tx, rx) = mpsc::channel();
    app.background_task = Some(rx);
    app.background_task_label = Some(REINJECT_LAST_LABEL);
    app.background_task_error_revision = Some(0);
    app.pipeline_stage = Some("injecting");
    app.runtime_error_revision = 1;
    app.last_runtime_error = Some("runtime stopped unexpectedly".to_owned());

    tx.send(BackgroundTaskResult {
        label: REINJECT_LAST_LABEL,
        command: "reinject auto".to_owned(),
        stdout: String::new(),
        stderr: String::new(),
        success: false,
        code: Some(1),
        error: Some("target activation failed".to_owned()),
    })
    .expect("background task result should be delivered");
    app.poll_background_task();

    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("runtime stopped unexpectedly")
    );
    assert!(!app.last_injection_failed);
    assert!(app.runtime_log.contains("target activation failed"));
}

#[test]
fn cloud_api_check_panic_is_reported_on_the_background_result_channel() {
    let check = CloudApiCheck {
        provider: "Nemotron 3.5 ASR".to_owned(),
        base_url: "grpc.nvcf.nvidia.com:443".to_owned(),
        model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
        api_key: "test-key".to_owned(),
        language: None,
        timeout_ms: 1_000,
    };

    let result = super::cloud_api_check_result(
        &check,
        "Nemotron gRPC API check".to_owned(),
        |_check| -> anyhow::Result<CloudApiCheckResult> {
            panic!("simulated gRPC transport panic")
        },
    );

    assert_eq!(result.label, "cloud API check");
    assert_eq!(result.command, "Nemotron gRPC API check");
    assert!(!result.success);
    assert!(result
        .error
        .as_deref()
        .is_some_and(|message| message.contains("cloud API check panicked")
            && message.contains("simulated gRPC transport panic")));
}
