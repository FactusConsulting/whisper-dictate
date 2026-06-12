//! Work driven from the UI: the synchronous doctor run plus the background
//! install/repair and cloud / post-processing API connectivity checks, and the
//! shared off-thread command runner with its result polling.

use super::*;
use crate::cloud_api::{check_cloud_api, check_post_api, CloudApiCheck, PostApiCheck};
use crate::runtime::{
    audio_devices_command, doctor_command, install_command, run_capture, test_audio_device_command,
    windows_command, WorkerCommand,
};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;

/// Background-task label for the worker's `--list-audio-devices` run. Matched in
/// `poll_background_task` to parse stdout into the Microphone picker options.
pub(in crate::ui) const LIST_AUDIO_DEVICES_LABEL: &str = "list audio devices";

/// Background-task label for the worker's `--list-windows` run. Matched in
/// `poll_background_task` to parse stdout into the Profiles tab window picker.
pub(in crate::ui) const LIST_WINDOWS_LABEL: &str = "list windows";

/// Background-task label for the worker's `--test-audio-device` run. Matched in
/// `poll_background_task` to parse stdout into the Microphone "Test" result.
pub(in crate::ui) const TEST_AUDIO_DEVICE_LABEL: &str = "test audio device";

impl WhisperDictateApp {
    pub(in crate::ui) fn run_doctor(&mut self) {
        let command = doctor_command();
        self.append_runtime_log(format!("[ui] doctor: {}", command.display()));
        match run_capture(&command) {
            Ok(output) => {
                self.append_runtime_output(output.stdout.trim_end());
                self.append_runtime_output(output.stderr.trim_end());
                if output.success() {
                    self.append_runtime_log("[ui] doctor passed");
                } else {
                    self.append_runtime_log(format!(
                        "[ui] doctor failed with code {}",
                        output
                            .code()
                            .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
                    ));
                }
            }
            Err(err) => self.append_runtime_log(format!("[ui] doctor failed to run: {err}")),
        }
    }

    pub(in crate::ui) fn run_install(&mut self) {
        self.run_background_command("install/repair", install_command());
    }

    /// Refresh the Microphone picker's device list by running the worker with
    /// `--list-audio-devices` off-thread. The captured stdout is parsed in
    /// `poll_background_task` once the run completes.
    pub(in crate::ui) fn run_list_audio_devices(&mut self) {
        self.run_background_command(LIST_AUDIO_DEVICES_LABEL, audio_devices_command());
    }

    /// Refresh the Profiles tab window list by running the worker with
    /// `--list-windows` off-thread. The captured stdout is parsed in
    /// `poll_background_task` once the run completes.
    pub(in crate::ui) fn run_list_windows(&mut self) {
        self.run_background_command(LIST_WINDOWS_LABEL, windows_command());
    }

    /// Dry-run test the currently-saved microphone by running the worker with
    /// `--test-audio-device "<name>"` off-thread (async, like Refresh devices, so
    /// the UI never blocks). The captured stdout is parsed into the inline ✓/⚠/✗
    /// result in `poll_background_task` once the run completes.
    pub(in crate::ui) fn run_test_audio_device(&mut self) {
        // Clear any previous result so the user sees the in-flight "Testing…"
        // state and never a stale outcome from the last device.
        self.device_test_result = None;
        let name = self.settings.audio_device.trim().to_owned();
        self.run_background_command(TEST_AUDIO_DEVICE_LABEL, test_audio_device_command(&name));
    }

    pub(in crate::ui) fn run_cloud_api_check(&mut self) {
        if self.background_task.is_some() {
            self.append_runtime_log("[ui] cloud API check skipped: another task is running");
            return;
        }

        let check = match CloudApiCheck::from_settings(&self.settings, &self.stt_api_key_input) {
            Ok(check) => check,
            Err(err) => {
                self.stt_api_key_status = format!("[ERROR] Cloud API check failed: {err}");
                self.append_runtime_log(format!("[ERROR] cloud API check failed: {err}"));
                return;
            }
        };
        self.append_runtime_log(format!(
            "[ui] cloud API check: {} {}",
            check.provider, check.model
        ));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = match check_cloud_api(&check) {
                Ok(result) => BackgroundTaskResult {
                    label: "cloud API check",
                    command: format!("{} /models", check.provider),
                    stdout: result.summary(),
                    stderr: String::new(),
                    success: result.model_available,
                    code: None,
                    error: None,
                },
                Err(err) => BackgroundTaskResult {
                    label: "cloud API check",
                    command: format!("{} /models", check.provider),
                    stdout: String::new(),
                    stderr: String::new(),
                    success: false,
                    code: None,
                    error: Some(err.to_string()),
                },
            };
            let _ = tx.send(result);
        });
        self.background_task = Some(rx);
        self.background_task_label = Some("cloud API check");
    }

    pub(in crate::ui) fn run_post_api_check(&mut self) {
        if self.background_task.is_some() {
            self.append_runtime_log("[ui] post API check skipped: another task is running");
            return;
        }

        let key = self.effective_post_api_key();
        let check = match PostApiCheck::from_settings(&self.settings, &key) {
            Ok(check) => check,
            Err(err) => {
                self.post_api_key_status = format!("[ERROR] Post API check failed: {err}");
                self.append_runtime_log(format!("[ERROR] post API check failed: {err}"));
                return;
            }
        };
        self.append_runtime_log(format!(
            "[ui] post API check: {} {}",
            check.provider, check.model
        ));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = match check_post_api(&check) {
                Ok(result) => BackgroundTaskResult {
                    label: "post API check",
                    command: format!("{} /chat/completions", check.provider),
                    stdout: result.summary(),
                    stderr: String::new(),
                    success: true,
                    code: None,
                    error: None,
                },
                Err(err) => BackgroundTaskResult {
                    label: "post API check",
                    command: format!("{} /chat/completions", check.provider),
                    stdout: String::new(),
                    stderr: String::new(),
                    success: false,
                    code: None,
                    error: Some(err.to_string()),
                },
            };
            let _ = tx.send(result);
        });
        self.background_task = Some(rx);
        self.background_task_label = Some("post API check");
    }

    pub(in crate::ui) fn effective_post_api_key(&self) -> String {
        let post_key = self.post_api_key_input.trim();
        if !post_key.is_empty() {
            return post_key.to_owned();
        }
        self.stt_api_key_input.trim().to_owned()
    }

    fn run_background_command(&mut self, label: &'static str, command: WorkerCommand) {
        if self.background_task.is_some() {
            self.append_runtime_log(format!("[ui] {label} skipped: another task is running"));
            return;
        }

        let display = command.display();
        self.append_runtime_log(format!("[ui] {label}: {display}"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = match run_capture(&command) {
                Ok(output) => {
                    let success = output.success();
                    let code = output.code();
                    BackgroundTaskResult {
                        label,
                        command: display,
                        stdout: output.stdout,
                        stderr: output.stderr,
                        success,
                        code,
                        error: None,
                    }
                }
                Err(err) => BackgroundTaskResult {
                    label,
                    command: display,
                    stdout: String::new(),
                    stderr: String::new(),
                    success: false,
                    code: None,
                    error: Some(err.to_string()),
                },
            };
            let _ = tx.send(result);
        });
        self.background_task = Some(rx);
        self.background_task_label = Some(label);
    }

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
            self.background_task = None;
            self.background_task_label = None;
            self.append_runtime_log(format!(
                "[ui] {} completed: {}",
                result.label, result.command
            ));
            if result.label == LIST_AUDIO_DEVICES_LABEL {
                self.apply_audio_device_listing(&result);
                return;
            }
            if result.label == LIST_WINDOWS_LABEL {
                self.apply_window_listing(&result);
                return;
            }
            if result.label == TEST_AUDIO_DEVICE_LABEL {
                self.apply_device_test(&result);
                return;
            }
            self.append_runtime_output(result.stdout.trim_end());
            self.append_runtime_output(result.stderr.trim_end());
            if let Some(error) = result.error {
                let message = format!("[ERROR] {} failed to run: {error}", result.label);
                self.set_api_check_status(result.label, &message);
                self.append_runtime_log(message);
            } else if result.success {
                // The generic [OK] line normally echoes the whole stdout as its
                // detail. For the benchmark that stdout is the full per-item JSONL
                // (already streamed line-by-line just above), so embedding it again
                // would duplicate a large blob into one giant log line and hurt UI
                // responsiveness/memory. Carry only the concise final
                // `[benchmark] …` summary line instead (if present).
                let detail = if result.label == RUN_BENCHMARK_LABEL {
                    benchmark_summary_line(&result.stdout).unwrap_or("")
                } else {
                    result.stdout.trim()
                };
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
    }

    fn set_api_check_status(&mut self, label: &str, message: &str) {
        match label {
            "cloud API check" => self.stt_api_key_status = message.to_owned(),
            "post API check" => self.post_api_key_status = message.to_owned(),
            _ => {}
        }
    }

    /// Handle a finished `--list-windows` run: parse stdout into the Profiles
    /// tab's window options, or report the failure via the runtime log.
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

// --- Run benchmark (appended; kept self-contained to ease merges) ------------
// The golden-corpus benchmark run. Its own `use`, label const and `impl` block
// live here at the end of the file so this feature can be added/removed without
// touching the import list or the main `impl` block above (which a parallel UI
// PR also edits).
use crate::runtime::benchmark_command;

/// Background-task label for the worker's `--run-benchmark` run. Mostly handled
/// by the generic `poll_background_task`: the per-item JSONL + the `[benchmark]`
/// summary line are streamed verbatim to the runtime log. The one special case
/// there is the `[OK]` completion line — it normally echoes the whole stdout as
/// its detail, which for the benchmark would re-dump the full JSONL into one
/// giant line, so it carries only `benchmark_summary_line` instead.
pub(in crate::ui) const RUN_BENCHMARK_LABEL: &str = "run benchmark";

/// Extract the concise final `[benchmark] …` summary line from the run's stdout
/// so the `[OK]` completion log line stays small instead of re-embedding the
/// whole per-item JSONL. Returns `None` when no summary line is present.
fn benchmark_summary_line(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .map(str::trim)
        .rfind(|line| line.starts_with("[benchmark]"))
}

impl WhisperDictateApp {
    /// Run the golden benchmark corpus off-thread via the worker's
    /// `--run-benchmark`. Same non-blocking pattern as "Refresh devices" so the
    /// (slow: model load + corpus) run never freezes the UI; gated on no other
    /// background task running. The captured stdout/stderr — including the final
    /// `[benchmark] …` summary line — lands in the runtime log when it completes.
    ///
    /// Prints an immediate "benchmark started" line (only when the run actually
    /// starts, i.e. no other task is in flight) so the button never feels dead:
    /// the model load + corpus pass is slow, and without this the runtime log
    /// would stay silent for many seconds after the click.
    pub(in crate::ui) fn run_benchmark(&mut self) {
        if self.background_task.is_none() {
            self.append_runtime_log("[ui] benchmark started — results appear here when finished");
        }
        self.run_background_command(RUN_BENCHMARK_LABEL, benchmark_command());
    }
}

#[cfg(test)]
mod benchmark_tests {
    use super::benchmark_summary_line;

    #[test]
    fn picks_the_last_benchmark_summary_line_and_ignores_jsonl() {
        let stdout = "\
{\"item\":1,\"wer\":0.1}
{\"item\":2,\"wer\":0.2}
[benchmark] 2/2 passed, avg WER 15.0%, avg CER 7.5%
";
        assert_eq!(
            benchmark_summary_line(stdout),
            Some("[benchmark] 2/2 passed, avg WER 15.0%, avg CER 7.5%"),
        );
    }

    #[test]
    fn returns_none_when_no_summary_line_present() {
        assert_eq!(benchmark_summary_line("{\"item\":1}\n"), None);
        assert_eq!(benchmark_summary_line(""), None);
    }
}
