//! Work driven from the UI: the background doctor run, microphone list + test,
//! and cloud / post-processing API connectivity checks,
//! plus the shared off-thread command runner with its result polling.
//!
//! Doctor and device listing are native. Reduced builds without
//! `audio-capture` report that the device enumerator is unavailable instead of
//! attempting a retired worker fallback.

use super::*;
use crate::cloud_api::{check_cloud_api, check_post_api, CloudApiCheck, PostApiCheck};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;

/// Background-task label for the worker's `--list-audio-devices` run. Matched in
/// `poll_background_task` to parse stdout into the Microphone picker options.
pub(in crate::ui) const LIST_AUDIO_DEVICES_LABEL: &str = "list audio devices";

/// Background-task label for native visible-window enumeration. Matched in
/// `poll_background_task` to parse the JSON contract into the Profiles picker.
pub(in crate::ui) const LIST_WINDOWS_LABEL: &str = "list windows";

#[cfg(test)]
type TestWindowEnumerator =
    fn() -> Result<Vec<crate::platform::window_enumeration::VisibleWindow>, String>;

#[cfg(test)]
static TEST_WINDOW_ENUMERATOR: std::sync::Mutex<Option<TestWindowEnumerator>> =
    std::sync::Mutex::new(None);

fn list_windows_for_ui() -> Result<Vec<crate::platform::window_enumeration::VisibleWindow>, String>
{
    #[cfg(test)]
    {
        let enumerator = *TEST_WINDOW_ENUMERATOR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(enumerator) = enumerator {
            return enumerator();
        }
    }
    crate::platform::window_enumeration::list_visible_windows()
}

/// Background-task label for the worker's `--test-audio-device` run. Matched in
/// `poll_background_task` to parse stdout into the Microphone "Test" result.
pub(in crate::ui) const TEST_AUDIO_DEVICE_LABEL: &str = "test audio device";

/// Background-task label for the native Rust doctor run. Matched in
/// `poll_background_task` and routed to `apply_doctor` so the checks text
/// lands in the runtime log without the generic `[OK]` handler embedding the
/// whole (multi-line) doctor output back into a single log line.
pub(in crate::ui) const DOCTOR_LABEL: &str = "doctor";

/// Background-task labels for the floating status surface's injection actions.
pub(in crate::ui) const REINJECT_LAST_LABEL: &str = "reinject last";
pub(in crate::ui) const RETRY_LAST_LABEL: &str = "retry last";

fn effective_reinject_xkb_layout(settings: &AppSettings) -> String {
    effective_xkb_layout(settings).unwrap_or_default()
}

impl WhisperDictateApp {
    /// Reinject the last transcript without blocking the egui frame. The
    /// action uses the guarded native injector and the mode captured with the
    /// utterance; a failure remains visible in the status surface and log.
    pub(in crate::ui) fn run_reinject_last(&mut self, label: &'static str) {
        if self.background_task.is_some() {
            self.append_runtime_log(format!("[ui] {label} skipped: another task is running"));
            return;
        }
        let Some(text) = self
            .last_transcript
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned)
        else {
            self.last_runtime_error = Some("No transcript is available yet.".to_owned());
            self.append_runtime_log(format!("[ui] {label} skipped: no transcript available"));
            return;
        };
        let mode = self
            .last_inject_mode
            .as_deref()
            .filter(|mode| !mode.trim().is_empty())
            .unwrap_or(self.settings.inject_mode.as_str())
            .to_owned();
        let target_title = self.last_target_title.clone();
        let target_process = self.last_target_process.clone();
        let target_id = self.last_target_id.clone();
        let xkb_layout = effective_reinject_xkb_layout(&self.settings);
        self.last_runtime_error = None;
        self.last_injection_failed = false;
        self.pipeline_stage = Some("injecting");
        self.pipeline_preview = None;
        self.append_runtime_log(format!("[ui] {label} started"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = crate::injection::reinject_text_for_ui(
                &text,
                &mode,
                &target_title,
                &target_process,
                &target_id,
                &xkb_layout,
            );
            let task = match result {
                Ok(()) => BackgroundTaskResult {
                    label,
                    command: format!("reinject {mode}"),
                    stdout: String::new(),
                    stderr: String::new(),
                    success: true,
                    code: Some(0),
                    error: None,
                },
                Err(error) => BackgroundTaskResult {
                    label,
                    command: format!("reinject {mode}"),
                    stdout: String::new(),
                    stderr: String::new(),
                    success: false,
                    code: Some(1),
                    error: Some(error.to_string()),
                },
            };
            let _ = tx.send(task);
        });
        self.background_task = Some(rx);
        self.background_task_label = Some(label);
    }

    /// Run the platform readiness matrix off-thread using the native Rust
    /// [`crate::doctor`] module — the same battery of checks the CLI verb
    /// (`whisper-dictate doctor`) runs. Emits a [`BackgroundTaskResult`] whose
    /// stdout is the rendered text output; [`apply_doctor`] streams it to the
    /// runtime log verbatim so the log stays scrapable.
    ///
    /// No feature gate: every doctor check works in a stock build (the
    /// `audio-input` check itself is already `#[cfg]`-gated inside
    /// [`crate::doctor`] and reports a WARN on non-`audio-capture` dev builds).
    pub(in crate::ui) fn run_doctor(&mut self) {
        if self.background_task.is_some() {
            self.append_runtime_log(format!(
                "[ui] {DOCTOR_LABEL} skipped: another task is running"
            ));
            return;
        }
        // Same log-line shape as every other background task (label + command)
        // so a reader can't tell native-vs-shellout apart in the log.
        let display = "doctor (native)".to_owned();
        self.append_runtime_log(format!("[ui] {DOCTOR_LABEL}: {display}"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let checks = crate::doctor::run_all_checks(None);
            let summary = crate::doctor::Summary::from(&checks);
            let stdout = crate::doctor::render_text_to_string(&checks, &summary);
            let success = summary.fail == 0;
            let _ = tx.send(BackgroundTaskResult {
                label: DOCTOR_LABEL,
                command: display,
                stdout,
                stderr: String::new(),
                // Exit code 0 iff no failing checks (WARN is non-blocking) —
                // mirrors [`crate::doctor::handle_doctor`]'s exit rule.
                success,
                code: Some(if success { 0 } else { 1 }),
                error: None,
            });
        });
        self.background_task = Some(rx);
        self.background_task_label = Some(DOCTOR_LABEL);
    }

    /// Refresh the Microphone picker's device list off-thread.
    ///
    /// On `audio-capture` builds (the shipping binary) this runs the native
    /// cpal enumeration in a background thread and synthesises a
    /// [`BackgroundTaskResult`] whose stdout is the same raw JSON array the
    /// historical worker's `--list-audio-devices` produced. Reduced builds
    /// fail visibly because they do not contain a native device enumerator.
    pub(in crate::ui) fn run_list_audio_devices(&mut self) {
        #[cfg(feature = "audio-capture")]
        {
            self.run_native_list_audio_devices();
        }
        #[cfg(not(feature = "audio-capture"))]
        {
            let message =
                "native microphone listing is unavailable in this reduced build; rebuild with \
                 --features audio-in-rust";
            self.append_runtime_log(format!("[ERROR] {message}"));
            self.device_test_result = Some(Err(message.to_owned()));
        }
    }

    /// Native cpal enumeration on a background thread. Synthesises a
    /// [`BackgroundTaskResult`] whose stdout is the JSON array the
    /// `apply_audio_device_listing` parser consumes — the parser stays
    /// authoritative.
    #[cfg(feature = "audio-capture")]
    fn run_native_list_audio_devices(&mut self) {
        if self.background_task.is_some() {
            self.append_runtime_log(format!(
                "[ui] {LIST_AUDIO_DEVICES_LABEL} skipped: another task is running"
            ));
            return;
        }
        let display = "devices list (native)".to_owned();
        self.append_runtime_log(format!("[ui] {LIST_AUDIO_DEVICES_LABEL}: {display}"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let stdout = crate::devices::list_input_devices_for_ui_json_line();
            let _ = tx.send(BackgroundTaskResult {
                label: LIST_AUDIO_DEVICES_LABEL,
                command: display,
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

    /// Refresh the Profiles tab window list through native Rust enumeration.
    /// Win32 calls run off-thread so the UI remains responsive; the resulting
    /// JSON uses the same contract the existing parser already consumes.
    pub(in crate::ui) fn run_list_windows(&mut self) {
        if self.background_task.is_some() {
            self.append_runtime_log(format!(
                "[ui] {LIST_WINDOWS_LABEL} skipped: another task is running"
            ));
            return;
        }
        let display = "windows list (native)".to_owned();
        self.append_runtime_log(format!("[ui] {LIST_WINDOWS_LABEL}: {display}"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = list_windows_for_ui().and_then(|windows| {
                serde_json::to_string(&windows)
                    .map_err(|err| format!("could not encode window list: {err}"))
            });
            let task_result = match result {
                Ok(stdout) => BackgroundTaskResult {
                    label: LIST_WINDOWS_LABEL,
                    command: display,
                    stdout,
                    stderr: String::new(),
                    success: true,
                    code: Some(0),
                    error: None,
                },
                Err(error) => BackgroundTaskResult {
                    label: LIST_WINDOWS_LABEL,
                    command: display,
                    stdout: String::new(),
                    stderr: String::new(),
                    success: false,
                    code: Some(1),
                    error: Some(error),
                },
            };
            let _ = tx.send(task_result);
        });
        self.background_task = Some(rx);
        self.background_task_label = Some(LIST_WINDOWS_LABEL);
    }

    /// Dry-run test the currently-saved microphone off-thread (async, like
    /// Refresh devices, so the UI never blocks). The captured stdout is parsed
    /// into the inline ✓/⚠/✗ result in `poll_background_task` once the run
    /// completes.
    ///
    /// On `audio-capture` builds (the shipping binary) this runs the native
    /// cpal probe in a background thread and synthesises a
    /// [`BackgroundTaskResult`] with a single-line JSON envelope the UI parser
    /// consumes — no subprocess, no Python. Step 2 of the `vp_device_test.py`
    /// retirement removed the stock-build Python shell-out fallback: dev
    /// builds without `audio-capture` log a "feature not enabled" message and
    /// leave the ✓/⚠/✗ pill empty. Release binaries always ship with
    /// `audio-capture`, so this only affects local dev builds.
    pub(in crate::ui) fn run_test_audio_device(&mut self) {
        // Clear any previous result so the user sees the in-flight "Testing…"
        // state and never a stale outcome from the last device.
        self.device_test_result = None;
        #[cfg(feature = "audio-capture")]
        {
            let name = self.settings.audio_device.trim().to_owned();
            self.run_native_device_test(name);
        }
        #[cfg(not(feature = "audio-capture"))]
        {
            self.append_runtime_log(
                "[ui] microphone test unavailable: this dev build lacks the \
                 `audio-capture` feature (rebuild with --features audio-capture)",
            );
        }
    }

    /// Native cpal probe on a background thread. Synthesises a
    /// [`BackgroundTaskResult`] with the JSON envelope as stdout so the
    /// generic `poll_background_task` → `apply_device_test` path parses it
    /// exactly like the Python worker's output — the parser stays authoritative.
    #[cfg(feature = "audio-capture")]
    fn run_native_device_test(&mut self, name: String) {
        if self.background_task.is_some() {
            self.append_runtime_log(format!(
                "[ui] {TEST_AUDIO_DEVICE_LABEL} skipped: another task is running"
            ));
            return;
        }
        // Log the equivalent of `command.display()` so the runtime log line
        // reads the same shape as every other background task (label +
        // command) — makes native-vs-shellout indistinguishable in the log.
        let display = format!("devices test {name:?} (native)");
        self.append_runtime_log(format!("[ui] {TEST_AUDIO_DEVICE_LABEL}: {display}"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = crate::audio::device_probe::probe_device(&name);
            let _ = tx.send(BackgroundTaskResult {
                label: TEST_AUDIO_DEVICE_LABEL,
                command: display,
                stdout: result.to_json_line(),
                stderr: String::new(),
                success: true,
                code: Some(0),
                error: None,
            });
        });
        self.background_task = Some(rx);
        self.background_task_label = Some(TEST_AUDIO_DEVICE_LABEL);
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
            if result.label == DOCTOR_LABEL {
                self.apply_doctor(&result);
                return;
            }
            if result.label == RUN_BENCHMARK_LABEL {
                self.apply_benchmark_results(&result);
                return;
            }
            if matches!(result.label, REINJECT_LAST_LABEL | RETRY_LAST_LABEL) {
                if self.pipeline_stage == Some("injecting") {
                    self.pipeline_stage = None;
                }
                if result.success {
                    self.last_runtime_error = None;
                    self.last_injection_failed = false;
                    self.settings_status = format!("{} completed.", result.label);
                    self.append_runtime_log(format!("[ui] {} completed", result.label));
                } else {
                    let detail = result
                        .error
                        .as_deref()
                        .or_else(|| {
                            (!result.stderr.trim().is_empty()).then_some(result.stderr.trim())
                        })
                        .unwrap_or("injection failed");
                    self.last_runtime_error = Some(detail.to_owned());
                    self.last_injection_failed = true;
                    self.append_runtime_log(format!("[ERROR] {} failed: {detail}", result.label));
                }
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

// --- Run benchmark (appended; kept self-contained to ease merges) ------------
// The golden-corpus benchmark run. Its own `use`, label const and `impl` block
// live here at the end of the file so this feature can be added/removed without
// touching the import list or the main `impl` block above (which a parallel UI
// PR also edits).

/// Background-task label for the native benchmark run. Routed in
/// `poll_background_task` to `apply_benchmark_results`, which parses the
/// captured JSONL + the trailing `[benchmark] …` summary line into the
/// System tab's digestible model.
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
    /// Run the golden benchmark corpus off-thread using the native Rust
    /// runner ([`crate::benchmark::native::run_to_writer`]) — the same code
    /// path the `whisper-dictate bench` CLI verb drives. Step 2 of the
    /// `vp_benchmark.py` retirement (#348) removed the Python subprocess:
    /// this now runs entirely in-process on the shipping build (feature
    /// combo `whisper-rs-local,audio-capture`), and reports a clear rebuild
    /// hint on stock dev builds.
    ///
    /// The runner's output (per-item JSONL + the final `[benchmark] …`
    /// summary line) is captured on the background thread and handed to
    /// [`apply_benchmark_results`] via a synthesised [`BackgroundTaskResult`]
    /// so the digestible view + runtime-log lines stay identical to the
    /// previous shell-out path — the parser stays authoritative.
    ///
    /// Prints an immediate "benchmark started" line (only when the run
    /// actually starts, i.e. no other task is in flight) so the button
    /// never feels dead: the model load + corpus pass is slow, and without
    /// this the runtime log would stay silent for many seconds after the
    /// click.
    pub(in crate::ui) fn run_benchmark(&mut self) {
        if self.background_task.is_some() {
            self.append_runtime_log(format!(
                "[ui] {RUN_BENCHMARK_LABEL} skipped: another task is running"
            ));
            return;
        }
        // Clear any previous parsed results so the digestible view shows the
        // in-flight state, not a stale table from the last run. Only when
        // the run actually starts (no other task in flight) — mirrors the
        // start line so a gated click leaves the prior results visible.
        self.benchmark_results = None;
        self.append_runtime_log("[ui] benchmark started — results appear here when finished");
        // Log the equivalent of `command.display()` so the runtime log line
        // reads the same shape as every other background task — makes
        // native-vs-shellout indistinguishable in the log.
        let display = "bench (native)".to_owned();
        self.append_runtime_log(format!("[ui] {RUN_BENCHMARK_LABEL}: {display}"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf: Vec<u8> = Vec::new();
            let outcome = crate::benchmark::native::run_to_writer(&mut buf);
            let stdout = String::from_utf8_lossy(&buf).into_owned();
            let (success, code, error) = match outcome {
                Ok(()) => (true, Some(0), None),
                Err(crate::benchmark::native::NativeBenchError::Unsupported(reason)) => (
                    false,
                    Some(1),
                    Some(format!(
                        "`wd bench` is only available in the shipping build \
                         ({reason}); rebuild with --features whisper-rs-local,audio-capture"
                    )),
                ),
                Err(crate::benchmark::native::NativeBenchError::Other(e)) => {
                    (false, Some(1), Some(format!("{e:#}")))
                }
            };
            let _ = tx.send(BackgroundTaskResult {
                label: RUN_BENCHMARK_LABEL,
                command: display,
                stdout,
                stderr: String::new(),
                success,
                code,
                error,
            });
        });
        self.background_task = Some(rx);
        self.background_task_label = Some(RUN_BENCHMARK_LABEL);
    }

    /// Handle a finished benchmark run: parse the captured per-item JSONL
    /// stdout into the digestible [`BenchmarkResults`] model the System tab
    /// renders (a coloured headline + a worst-WER-first table), AND preserve the
    /// exact runtime-log behaviour the user already relied on — the per-item
    /// JSONL streamed verbatim plus the concise final `[benchmark] …` summary line
    /// (re-using `benchmark_summary_line` so a large blob is never re-embedded in
    /// one giant `[OK]` line). A run failure (worker couldn't even start) clears
    /// the model and logs the error, mirroring the generic failure path.
    pub(in crate::ui) fn apply_benchmark_results(&mut self, result: &BackgroundTaskResult) {
        // Stream the raw output to the log first, unchanged: the per-item JSONL
        // (and stderr) the user has always seen stays in the runtime log so the
        // digestible view is purely additive and the raw remains inspectable.
        self.append_runtime_output(result.stdout.trim_end());
        self.append_runtime_output(result.stderr.trim_end());

        if let Some(error) = &result.error {
            // The worker couldn't run at all — there is no stdout to parse. Clear
            // any stale model and surface the failure like the generic path did.
            self.benchmark_results = None;
            self.append_runtime_log(format!("[ERROR] {} failed to run: {error}", result.label));
            return;
        }

        // Parse the captured stdout into the model regardless of exit code — a
        // non-zero exit can still carry usable per-item rows worth showing.
        let results = parse_benchmark_results(&result.stdout);

        if result.success {
            // Preserve the original `[OK] … passed: [benchmark] …` line: carry only
            // the concise summary line, never the whole JSONL blob.
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

        // Log the digestible one-line headline (the localized view lives in the
        // System tab) so even the log reader gets the at-a-glance result.
        if !results.is_empty() {
            self.append_runtime_log(format!(
                "[ui] benchmark: {}",
                benchmark_results_log_detail(&results)
            ));
        }
        self.benchmark_results = Some(results);
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

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod window_task_tests;
