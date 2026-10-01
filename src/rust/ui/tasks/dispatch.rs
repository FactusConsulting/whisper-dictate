//! Completion routing for background tasks.

use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn poll_background_task(&mut self) {
        let Some(rx) = self.background_task.as_ref() else {
            return;
        };

        let result = match rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(BackgroundTaskResult {
                label: self.background_task_label.unwrap_or("background task"),
                command: String::new(),
                stdout: String::new(),
                stderr: String::new(),
                success: false,
                code: None,
                error: Some("background task stopped without reporting a result".to_owned()),
            }),
        };

        if let Some(result) = result {
            let task_error_revision = self.background_task_error_revision.take();
            self.background_task = None;
            self.background_task_label = None;
            self.nemotron_probe_active = None;
            self.nemotron_probe_settings = None;
            self.append_runtime_log(format!(
                "[ui] {} completed: {}",
                result.label, result.command
            ));
            self.apply_completed_background_task(result, task_error_revision);
        }
    }

    fn apply_completed_background_task(
        &mut self,
        result: BackgroundTaskResult,
        task_error_revision: Option<u64>,
    ) {
        match result.label {
            LIST_AUDIO_DEVICES_LABEL => self.apply_audio_device_listing(&result),
            LIST_WINDOWS_LABEL => self.apply_window_listing(&result),
            TEST_AUDIO_DEVICE_LABEL => self.apply_device_test(&result),
            RECORD_CORPUS_ITEM_LABEL => self.apply_corpus_record(&result),
            DOCTOR_LABEL => self.apply_doctor(&result),
            RUN_BENCHMARK_LABEL => self.apply_benchmark_results(&result),
            REINJECT_LAST_LABEL | RETRY_LAST_LABEL => {
                self.apply_completed_injection(&result, task_error_revision)
            }
            _ => self.apply_completed_command(result),
        }
    }

    fn apply_completed_injection(
        &mut self,
        result: &BackgroundTaskResult,
        task_error_revision: Option<u64>,
    ) {
        if self.pipeline_stage == Some("injecting") {
            self.pipeline_stage = None;
        }
        if result.success {
            let runtime_error_is_unchanged =
                task_error_revision == Some(self.runtime_error_revision);
            if runtime_error_is_unchanged {
                self.last_runtime_error_from_runtime = false;
                self.last_runtime_error = None;
                self.last_injection_failed = false;
            }
            if self.last_runtime_error.is_none() {
                self.settings_status = format!("{} completed.", result.label);
                self.append_runtime_log(format!("[ui] {} completed", result.label));
            }
        } else {
            let detail = result
                .error
                .as_deref()
                .or_else(|| (!result.stderr.trim().is_empty()).then_some(result.stderr.trim()))
                .unwrap_or("injection failed");
            let runtime_error_is_unchanged =
                task_error_revision.is_none_or(|revision| revision == self.runtime_error_revision);
            if runtime_error_is_unchanged {
                self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
                self.last_runtime_error_from_runtime = false;
                self.last_runtime_error = Some(detail.to_owned());
                self.last_injection_failed = true;
            }
            self.append_runtime_log(format!("[ERROR] {} failed: {detail}", result.label));
        }
    }

    fn apply_completed_command(&mut self, result: BackgroundTaskResult) {
        self.append_runtime_output(result.stdout.trim_end());
        self.append_runtime_output(result.stderr.trim_end());
        if let Some(error) = result.error {
            let message = format!("[ERROR] {} failed to run: {error}", result.label);
            self.set_api_check_status(result.label, &message);
            self.append_runtime_log(message);
        } else if result.success {
            // The benchmark run is routed to `apply_benchmark_results` above
            // (its stdout is the full per-item JSONL, parsed into the
            // digestible view + the concise `[benchmark] …` summary line), so
            // it never reaches this generic path. Other tasks echo their
            // (small) stdout as the `[OK]` detail.
            let detail = result.stdout.trim();
            let message = if detail.is_empty() {
                format!("[OK] {} passed", result.label)
            } else {
                format!("[OK] {} passed: {detail}", result.label)
            };
            self.set_api_check_status(result.label, &message);
            self.append_runtime_log(message);
        } else {
            let detail = result.stdout.trim();
            let mut message = format!(
                "[ERROR] {} failed with code {}",
                result.label,
                result
                    .code
                    .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
            );
            if !detail.is_empty() {
                message.push_str(": ");
                message.push_str(detail);
            }
            self.set_api_check_status(result.label, &message);
            self.append_runtime_log(message);
        }
    }

    fn set_api_check_status(&mut self, label: &str, message: &str) {
        match label {
            "cloud API check" => self.stt_api_key_status = message.to_owned(),
            "post API check" => self.post_api_key_status = message.to_owned(),
            _ => {}
        }
    }

    /// Handle finished native window enumeration: parse its JSON into the
    /// Profiles tab's options, or report the failure via the runtime log.
    fn apply_window_listing(&mut self, result: &BackgroundTaskResult) {
        if let Some(error) = &result.error {
            let message = format!("Could not list windows: {error}");
            self.append_runtime_log(format!("[ERROR] {message}"));
            return;
        }
        match parse_windows_json(&result.stdout) {
            Ok(entries) => {
                let count = entries.len();
                self.window_options = entries.into_iter().map(|e| (e.title, e.process)).collect();
                self.append_runtime_log(format!("[ui] window list refreshed: {count} window(s)"));
            }
            Err(error) => {
                let message = format!("Could not read window list: {error}");
                self.append_runtime_log(format!("[ERROR] {message}"));
                if !result.stderr.trim().is_empty() {
                    self.append_runtime_output(result.stderr.trim_end());
                }
            }
        }
    }

    /// Handle a finished native doctor run: stream the rendered text output
    /// to the runtime log verbatim (so the check-by-check matrix stays
    /// readable) plus a concise pass/fail line. A channel failure is reported via
    /// the runtime log so the button is never silent.
    fn apply_doctor(&mut self, result: &BackgroundTaskResult) {
        if let Some(error) = &result.error {
            self.append_runtime_log(format!("[ui] {} failed to run: {error}", result.label));
            return;
        }
        self.append_runtime_output(result.stdout.trim_end());
        if !result.stderr.trim().is_empty() {
            self.append_runtime_output(result.stderr.trim_end());
        }
        if result.success {
            self.append_runtime_log(format!("[ui] {} passed", result.label));
        } else {
            self.append_runtime_log(format!(
                "[ui] {} failed with code {}",
                result.label,
                result
                    .code
                    .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
            ));
        }
    }

    /// Handle a finished `--test-audio-device` run: parse the single JSON result
    /// object into the inline ✓/⚠/✗ display model (stored in `device_test_result`)
    /// and log the outcome. A run failure (worker couldn't even start) is stored
    /// as an `Err` so the picker shows it instead of silently doing nothing.
    fn apply_device_test(&mut self, result: &BackgroundTaskResult) {
        if let Some(error) = &result.error {
            let message = format!("Could not test microphone: {error}");
            self.append_runtime_log(format!("[ERROR] {message}"));
            self.device_test_result = Some(Err(message));
            return;
        }
        match parse_device_test_json(&result.stdout) {
            Ok(display) => {
                self.append_runtime_log(format!(
                    "[ui] microphone test: {}",
                    device_test_log_detail(&display)
                ));
                self.device_test_result = Some(Ok(display));
            }
            Err(error) => {
                let message = format!("Could not read microphone test result: {error}");
                self.append_runtime_log(format!("[ERROR] {message}"));
                if !result.stderr.trim().is_empty() {
                    self.append_runtime_output(result.stderr.trim_end());
                }
                self.device_test_result = Some(Err(message));
            }
        }
    }

    /// Handle a finished `--list-audio-devices` run: parse stdout into the
    /// Microphone combo options, or report the failure via the settings status
    /// line and the runtime log without disturbing the saved device value.
    fn apply_audio_device_listing(&mut self, result: &BackgroundTaskResult) {
        if let Some(error) = &result.error {
            let message = format!("Could not list audio devices: {error}");
            self.settings_status = message.clone();
            self.append_runtime_log(format!("[ERROR] {message}"));
            return;
        }
        match parse_audio_devices_json(&result.stdout) {
            Ok(options) => {
                let count = options.len();
                let labels = options
                    .iter()
                    .map(|d| d.label.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.audio_device_options = options.into_iter().map(|d| d.value).collect();
                self.settings_status = format!("Found {count} input device(s).");
                let detail = if labels.is_empty() {
                    String::new()
                } else {
                    format!(": {labels}")
                };
                self.append_runtime_log(format!(
                    "[ui] microphone list refreshed: {count} device(s){detail}"
                ));
            }
            Err(error) => {
                let message = format!("Could not read audio device list: {error}");
                self.settings_status = message.clone();
                self.append_runtime_log(format!("[ERROR] {message}"));
                if !result.stderr.trim().is_empty() {
                    self.append_runtime_output(result.stderr.trim_end());
                }
            }
        }
    }
}
