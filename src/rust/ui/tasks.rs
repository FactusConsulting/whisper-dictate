//! Work driven from the UI: the synchronous doctor run plus the background
//! install/repair and cloud / post-processing API connectivity checks, and the
//! shared off-thread command runner with its result polling.

use super::*;
use crate::cloud_api::{check_cloud_api, check_post_api, CloudApiCheck, PostApiCheck};
#[cfg(feature = "audio-in-rust")]
use serde_json;
use std::sync::mpsc::{self, TryRecvError};
use std::thread;

/// Background-task label for the input-device enumeration run (Rust
/// `devices::list_input_devices` when compiled with `audio-in-rust`; Python
/// `--list-audio-devices` otherwise). Matched in `poll_background_task` to
/// parse stdout into the Microphone picker options.
pub(in crate::ui) const LIST_AUDIO_DEVICES_LABEL: &str = "list audio devices";

/// Background-task label for the worker's `--list-windows` run. Matched in
/// `poll_background_task` to parse stdout into the Profiles tab window picker.
pub(in crate::ui) const LIST_WINDOWS_LABEL: &str = "list windows";

/// Background-task label for the worker's `--test-audio-device` run. Matched in
/// `poll_background_task` to parse stdout into the Microphone "Test" result.
pub(in crate::ui) const TEST_AUDIO_DEVICE_LABEL: &str = "test audio device";

/// Serialise a slice of [`crate::devices::DeviceInfo`] to a JSON array string
/// that [`parse_audio_devices_json`] can consume. On serialisation error
/// (should never happen for well-formed `DeviceInfo`) returns `"[]"` so the UI
/// always gets a valid (empty) array rather than an error state.
#[cfg(feature = "audio-in-rust")]
fn serialize_devices_for_ui(devices: &[crate::devices::DeviceInfo]) -> String {
    serde_json::to_string(devices).unwrap_or_else(|_| "[]".to_owned())
}

impl WhisperDictateApp {
    /// Wave 8 Part 2: the pre-v1.20 doctor shelled out to the Python
    /// worker via `python -m whisper_dictate.runtime --doctor`; the
    /// Python bundle is gone in v1.20 and a native-Rust doctor is a
    /// follow-up. Meanwhile this button surfaces a friendly log entry
    /// so the UI stays honest.
    pub(in crate::ui) fn run_doctor(&mut self) {
        self.append_runtime_log(
            "[ui] doctor: removed in v1.20 with the Python worker; the native-Rust doctor \
             is tracked as a follow-up to #348. In the meantime use `whisper-dictate models list`, \
             `whisper-dictate --version`, and `whisper-dictate config show` for diagnostics.",
        );
    }

    /// Wave 8 Part 2: `install` no longer runs `pip install` — the
    /// packaged binary carries every runtime dependency. Kept as a log
    /// entry so the button doesn't feel dead.
    pub(in crate::ui) fn run_install(&mut self) {
        self.append_runtime_log(
            "[ui] install/repair: removed in v1.20. The packaged binary ships every runtime \
             dependency, so there is no venv or pip step. Download a model via \
             `whisper-dictate models download tiny.en` when needed.",
        );
    }

    /// Refresh the Microphone picker's device list by enumerating input
    /// devices in-process via `crate::devices::list_input_devices()`.
    /// Wave 8 Part 2 dropped the Python fallback: on stock builds without
    /// `audio-in-rust` the picker stays empty (documented follow-up).
    pub(in crate::ui) fn run_list_audio_devices(&mut self) {
        #[cfg(feature = "audio-in-rust")]
        {
            if self.background_task.is_some() {
                self.append_runtime_log("[ui] list audio devices skipped: another task is running");
                return;
            }
            self.append_runtime_log("[ui] list audio devices: rust:devices list");
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let devices = crate::devices::list_input_devices();
                let stdout = serialize_devices_for_ui(&devices);
                let _ = tx.send(BackgroundTaskResult {
                    label: LIST_AUDIO_DEVICES_LABEL,
                    command: "rust:devices list".to_owned(),
                    stdout,
                    stderr: String::new(),
                    success: true,
                    code: Some(0),
                    error: None,
                });
            });
            self.background_task = Some(rx);
            self.background_task_label = Some(LIST_AUDIO_DEVICES_LABEL);
        }
        #[cfg(not(feature = "audio-in-rust"))]
        self.append_runtime_log(
            "[ui] list audio devices: this build was compiled without --features audio-in-rust; \
             rebuild with the feature to enumerate microphones.",
        );
    }

    /// Wave 8 Part 2: `--list-windows` was a Python-worker flag; the
    /// Rust replacement is a follow-up. Surface a friendly log.
    pub(in crate::ui) fn run_list_windows(&mut self) {
        self.append_runtime_log(
            "[ui] list windows: removed in v1.20 with the Python worker. A native-Rust window \
             enumerator is tracked as a follow-up.",
        );
    }

    /// Wave 8 Part 2: `--test-audio-device` was a Python-worker flag;
    /// the Rust replacement is a follow-up. Reset the previous result
    /// so no stale ✓/⚠/✗ is shown.
    pub(in crate::ui) fn run_test_audio_device(&mut self) {
        self.device_test_result = None;
        self.append_runtime_log(
            "[ui] test audio device: removed in v1.20 with the Python worker. Rebuild with the \
             audio-in-rust feature and run `whisper-dictate devices` to inspect microphones.",
        );
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

    // Wave 8 Part 2: `run_background_command` shelled out to
    // `run_capture(command)` which spawned the Python worker with a
    // subcommand flag (`--doctor`, `--list-audio-devices`, etc.). All
    // those flags belonged to the deleted Python bundle; the generic
    // "spawn a worker command and stream its output to the UI log"
    // helper has no live caller now, so it was removed together with
    // the callers.

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
            if result.label == RECORD_CORPUS_ITEM_LABEL {
                self.apply_corpus_record(&result);
                return;
            }
            if result.label == RUN_BENCHMARK_LABEL {
                self.apply_benchmark_results(&result);
                return;
            }
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

// --- Run benchmark ----------------------------------------------------------
// Wave 8 Part 2: the pre-v1.20 benchmark run shelled out to the Python
// worker via `--run-benchmark`; a native-Rust benchmark harness is
// tracked as a follow-up. Meanwhile the UI button surfaces a friendly
// log entry so the button doesn't feel dead.

/// Retained label so any lingering match-on-label consumers keep
/// compiling (see the `poll_background_task` `RUN_BENCHMARK_LABEL`
/// arm). Nothing actually schedules a run against it any more.
pub(in crate::ui) const RUN_BENCHMARK_LABEL: &str = "run benchmark";

/// Extract the concise final `[benchmark] …` summary line — kept
/// alongside the label so future Rust-benchmark wiring can plug it
/// straight back in.
fn benchmark_summary_line(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .map(str::trim)
        .rfind(|line| line.starts_with("[benchmark]"))
}

impl WhisperDictateApp {
    /// Wave 8 Part 2 stub — logs a "removed" hint.
    pub(in crate::ui) fn run_benchmark(&mut self) {
        self.benchmark_results = None;
        self.append_runtime_log(
            "[ui] benchmark: removed in v1.20 with the Python worker. A native-Rust benchmark \
             harness is tracked as a follow-up to #348.",
        );
    }

    /// Wave 8 Part 2 stub — no live scheduler emits a
    /// `RUN_BENCHMARK_LABEL` result, so this function never runs. Kept
    /// so `poll_background_task`'s match arm still resolves.
    pub(in crate::ui) fn apply_benchmark_results(&mut self, result: &BackgroundTaskResult) {
        self.append_runtime_output(result.stdout.trim_end());
        self.append_runtime_output(result.stderr.trim_end());
        if let Some(error) = &result.error {
            self.benchmark_results = None;
            self.append_runtime_log(format!("[ERROR] {} failed to run: {error}", result.label));
            return;
        }
        let results = parse_benchmark_results(&result.stdout);
        if result.success {
            let detail = benchmark_summary_line(&result.stdout).unwrap_or("");
            let message = if detail.is_empty() {
                format!("[OK] {} passed", result.label)
            } else {
                format!("[OK] {} passed: {detail}", result.label)
            };
            self.append_runtime_log(message);
        } else {
            let mut message = format!(
                "[ERROR] {} failed with code {}",
                result.label,
                result
                    .code
                    .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
            );
            if let Some(summary) = benchmark_summary_line(&result.stdout) {
                message.push_str(": ");
                message.push_str(summary);
            }
            self.append_runtime_log(message);
        }
        if !results.is_empty() {
            self.append_runtime_log(format!(
                "[ui] benchmark: {}",
                benchmark_results_log_detail(&results)
            ));
        }
        self.benchmark_results = Some(results);
    }
}

#[cfg(all(test, feature = "audio-in-rust"))]
mod devices_reroute_tests {
    use super::*;
    use crate::devices::DeviceInfo;

    fn make_device(index: usize, name: &str, default: bool) -> DeviceInfo {
        DeviceInfo {
            index,
            name: name.to_owned(),
            max_input_channels: 2,
            sample_rates: (16_000, 48_000),
            default,
        }
    }

    /// `serialize_devices_for_ui` must produce a JSON array whose elements
    /// carry at least `name` and `default` fields — the exact contract
    /// `parse_audio_devices_json` requires.
    #[test]
    fn serialised_device_info_round_trips_through_audio_devices_parser() {
        let devices = vec![
            make_device(0, "Mic A", true),
            make_device(1, "Mic B", false),
        ];
        let json = serialize_devices_for_ui(&devices);
        let options = parse_audio_devices_json(&json)
            .expect("parse_audio_devices_json must accept DeviceInfo JSON");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].value, "Mic A");
        assert_eq!(options[0].label, "Mic A (default)");
        assert_eq!(options[1].value, "Mic B");
        assert_eq!(options[1].label, "Mic B");
    }

    /// An empty device list must serialise to `[]` and produce zero options
    /// (not an error).
    #[test]
    fn serialised_empty_device_list_yields_no_options() {
        let json = serialize_devices_for_ui(&[]);
        let options = parse_audio_devices_json(&json).expect("empty array must be valid");
        assert!(options.is_empty());
    }

    /// `run_list_audio_devices` must start a background task (setting both
    /// `background_task` and `background_task_label`) without panicking. We
    /// don't wait for the thread to finish because the real `list_input_devices`
    /// may take a moment and CI hosts may have no audio hardware — the goal is
    /// just to confirm the Rust path is wired up and doesn't touch Python.
    #[test]
    fn run_list_audio_devices_starts_background_task_on_rust_path() {
        use crate::config::AppSettings;
        // tasks is a direct child of crate::ui; super::super resolves to
        // crate::ui where test_support lives.
        use super::super::test_support::test_app;
        let mut app = test_app(AppSettings::default());
        assert!(
            app.background_task.is_none(),
            "precondition: no task in flight"
        );
        app.run_list_audio_devices();
        assert!(
            app.background_task.is_some(),
            "background task must have been started"
        );
        assert_eq!(
            app.background_task_label,
            Some(LIST_AUDIO_DEVICES_LABEL),
            "task label must be the list-audio-devices label"
        );
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
