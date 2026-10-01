use super::*;

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

use crate::ui::tasks::{
    DOCTOR_LABEL, LIST_AUDIO_DEVICES_LABEL, LIST_WINDOWS_LABEL, RETRY_LAST_LABEL,
    TEST_AUDIO_DEVICE_LABEL,
};

fn result_for(label: &'static str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        label,
        command: "fixture".to_owned(),
        stdout: String::new(),
        stderr: String::new(),
        success: true,
        code: Some(0),
        error: None,
    }
}

#[test]
fn stale_retry_success_or_failure_cannot_replace_a_newer_runtime_error() {
    for success in [true, false] {
        let mut app = test_app(AppSettings::default());
        app.runtime_error_revision = 8;
        app.background_task_error_revision = Some(7);
        app.last_runtime_error = Some("newer runtime failure".to_owned());
        app.last_runtime_error_from_runtime = true;
        app.last_injection_failed = true;
        app.pipeline_stage = Some("injecting");
        let (tx, rx) = mpsc::channel();
        app.background_task = Some(rx);
        app.background_task_label = Some(RETRY_LAST_LABEL);
        let mut result = result_for(RETRY_LAST_LABEL);
        result.success = success;
        result.error = (!success).then(|| "stale retry failure".to_owned());
        tx.send(result).unwrap();
        app.poll_background_task();
        assert!(app.background_task.is_none());
        assert!(app.background_task_error_revision.is_none());
        assert_eq!(app.pipeline_stage, None);
        assert_eq!(app.runtime_error_revision, 8);
        assert_eq!(
            app.last_runtime_error.as_deref(),
            Some("newer runtime failure")
        );
        assert!(app.last_runtime_error_from_runtime);
        assert!(app.last_injection_failed);
    }
}

fn complete(app: &mut WhisperDictateApp, result: BackgroundTaskResult) {
    let (tx, rx) = mpsc::channel();
    app.background_task = Some(rx);
    app.background_task_label = Some(result.label);
    app.background_task_error_revision = Some(app.runtime_error_revision);
    app.nemotron_probe_active = Some(Arc::new(AtomicBool::new(true)));
    app.nemotron_probe_settings = Some(crate::ui::local_nemotron_probe_settings(&app.settings));
    tx.send(result).unwrap();
    app.poll_background_task();
    assert!(app.background_task.is_none());
    assert!(app.background_task_label.is_none());
    assert!(app.background_task_error_revision.is_none());
    assert!(app.nemotron_probe_active.is_none());
    assert!(app.nemotron_probe_settings.is_none());
}

#[test]
fn empty_receiver_keeps_the_task_pending_then_disconnect_reports_failure() {
    let mut app = test_app(AppSettings::default());
    let (tx, rx) = mpsc::channel();
    app.background_task = Some(rx);
    app.background_task_label = Some("cloud API check");
    app.poll_background_task();
    assert!(app.background_task.is_some());
    assert_eq!(app.background_task_label, Some("cloud API check"));
    drop(tx);
    app.poll_background_task();
    assert!(app.background_task.is_none());
    assert!(app
        .stt_api_key_status
        .contains("stopped without reporting a result"));
}

#[test]
fn generic_success_reports_empty_and_nonempty_details_to_the_right_surface() {
    for (label, detail) in [("cloud API check", ""), ("post API check", "reachable")] {
        let mut app = test_app(AppSettings::default());
        let mut result = result_for(label);
        result.stdout = format!(" {detail} \n");
        complete(&mut app, result);
        let status = if label == "cloud API check" {
            &app.stt_api_key_status
        } else {
            &app.post_api_key_status
        };
        let expected = if detail.is_empty() {
            format!("[OK] {label} passed")
        } else {
            format!("[OK] {label} passed: {detail}")
        };
        assert_eq!(status, &expected);
        assert!(app.runtime_log.contains(&expected));
    }
}

#[test]
fn generic_launch_and_exit_failures_are_reported_without_restarting_runtime() {
    for (error, code) in [(Some("cannot launch"), None), (None, Some(7)), (None, None)] {
        let mut app = test_app(AppSettings::default());
        let mut result = result_for("cloud API check");
        result.success = false;
        result.error = error.map(str::to_owned);
        result.code = code;
        result.stdout = "response detail".to_owned();
        result.stderr = "diagnostic detail\n".to_owned();
        complete(&mut app, result);
        assert!(app
            .stt_api_key_status
            .starts_with("[ERROR] cloud API check"));
        let detail = error.map(str::to_owned).unwrap_or_else(|| {
            format!(
                "code {}",
                code.map_or_else(|| "unknown".to_owned(), |n| n.to_string())
            )
        });
        assert!(app.stt_api_key_status.contains(&detail));
        assert!(app.runtime_log.contains("diagnostic detail"));
        assert_eq!(app.runtime_state, RuntimeState::Stopped);
    }
}

#[test]
fn retry_success_clears_its_own_error_and_injecting_stage() {
    let mut app = test_app(AppSettings::default());
    app.last_runtime_error = Some("old injection failure".to_owned());
    app.last_runtime_error_from_runtime = true;
    app.last_injection_failed = true;
    app.pipeline_stage = Some("injecting");
    complete(&mut app, result_for(RETRY_LAST_LABEL));
    assert_eq!(app.pipeline_stage, None);
    assert!(app.last_runtime_error.is_none());
    assert!(!app.last_runtime_error_from_runtime);
    assert!(!app.last_injection_failed);
    assert_eq!(app.settings_status, "retry last completed.");
}

#[test]
fn retry_failure_uses_stderr_or_a_fallback_and_advances_error_revision() {
    for stderr in ["helper unavailable", ""] {
        let mut app = test_app(AppSettings::default());
        let previous = app.runtime_error_revision;
        app.pipeline_stage = Some("injecting");
        let mut result = result_for(RETRY_LAST_LABEL);
        result.success = false;
        result.stderr = stderr.to_owned();
        complete(&mut app, result);
        assert_eq!(app.pipeline_stage, None);
        assert_eq!(app.runtime_error_revision, previous.wrapping_add(1));
        assert_eq!(
            app.last_runtime_error.as_deref(),
            Some(if stderr.is_empty() {
                "injection failed"
            } else {
                stderr
            })
        );
        assert!(app.last_injection_failed);
    }
}

#[test]
fn window_listing_failures_preserve_the_previous_picker_options() {
    for error in [Some("enumeration unavailable"), None] {
        let mut app = test_app(AppSettings::default());
        let previous = vec![("previous".to_owned(), "editor".to_owned())];
        app.window_options = previous.clone();
        let mut result = result_for(LIST_WINDOWS_LABEL);
        result.error = error.map(str::to_owned);
        result.stdout = "not JSON".to_owned();
        result.stderr = "window diagnostic".to_owned();
        complete(&mut app, result);
        assert_eq!(app.window_options, previous);
        assert!(app.runtime_log.contains("Could not"));
        if error.is_none() {
            assert!(app.runtime_log.contains("window diagnostic"));
        }
    }
}

#[test]
fn doctor_results_keep_check_output_and_the_pass_or_failure_summary() {
    for (error, success, code) in [
        (Some("doctor unavailable"), false, None),
        (None, true, Some(0)),
        (None, false, Some(3)),
        (None, false, None),
    ] {
        let mut app = test_app(AppSettings::default());
        let mut result = result_for(DOCTOR_LABEL);
        result.error = error.map(str::to_owned);
        result.success = success;
        result.code = code;
        result.stdout = "check matrix".to_owned();
        result.stderr = "doctor diagnostic".to_owned();
        complete(&mut app, result);
        if error.is_some() {
            assert!(app.runtime_log.contains("doctor unavailable"));
        } else {
            assert!(app.runtime_log.contains("check matrix"));
            assert!(app.runtime_log.contains("doctor diagnostic"));
            assert!(app.runtime_log.contains(if success {
                "doctor passed"
            } else {
                "doctor failed with code"
            }));
        }
    }
}

#[test]
fn microphone_test_routes_valid_and_failed_results_to_the_inline_display() {
    for (error, stdout) in [
        (Some("cannot open"), ""),
        (None, "malformed"),
        (
            None,
            r#"{"usable":true,"endpoint":"wasapi","samplerate":16000}"#,
        ),
    ] {
        let mut app = test_app(AppSettings::default());
        let mut result = result_for(TEST_AUDIO_DEVICE_LABEL);
        result.error = error.map(str::to_owned);
        result.stdout = stdout.to_owned();
        result.stderr = "microphone diagnostic".to_owned();
        complete(&mut app, result);
        let display = app.device_test_result.as_ref().unwrap();
        if error.is_some() || stdout == "malformed" {
            assert!(display.is_err());
            assert!(app.runtime_log.contains("Could not"));
        } else {
            assert_eq!(display.as_ref().unwrap().outcome, DeviceTestOutcome::Works);
            assert!(app.runtime_log.contains("microphone test"));
        }
    }
}

#[test]
fn microphone_listing_reports_failures_without_changing_the_saved_selection() {
    for error in [Some("enumeration failed"), None] {
        let settings = AppSettings {
            audio_device: "saved microphone".to_owned(),
            ..Default::default()
        };
        let mut app = test_app(settings);
        app.audio_device_options = vec!["previous microphone".to_owned()];
        let mut result = result_for(LIST_AUDIO_DEVICES_LABEL);
        result.error = error.map(str::to_owned);
        result.stdout = "not a list".to_owned();
        result.stderr = "list diagnostic".to_owned();
        complete(&mut app, result);
        assert_eq!(app.settings.audio_device, "saved microphone");
        assert_eq!(app.audio_device_options, ["previous microphone"]);
        assert!(app.settings_status.contains("Could not"));
        if error.is_none() {
            assert!(app.runtime_log.contains("list diagnostic"));
        }
    }
}

#[test]
fn microphone_listing_handles_empty_and_named_results() {
    for stdout in ["[]", r#"[{"name":"USB microphone","default":true}]"#] {
        let mut app = test_app(AppSettings::default());
        let mut result = result_for(LIST_AUDIO_DEVICES_LABEL);
        result.stdout = stdout.to_owned();
        complete(&mut app, result);
        if stdout == "[]" {
            assert!(app.audio_device_options.is_empty());
            assert_eq!(app.settings_status, "Found 0 input device(s).");
        } else {
            assert_eq!(app.audio_device_options, ["USB microphone"]);
            assert!(app.runtime_log.contains("USB microphone (default)"));
            assert_eq!(app.settings_status, "Found 1 input device(s).");
        }
    }
}
