//! Work driven from the UI: the background doctor run, microphone list + test,
//! and cloud / post-processing API connectivity checks,
//! plus the shared off-thread command runner with its result polling.
//!
//! Doctor and device listing are native. Reduced builds without
//! `audio-capture` report that the device enumerator is unavailable instead of
//! attempting a retired worker fallback.

mod benchmark;
mod cloud_checks;
mod dispatch;

use super::*;
use crate::cloud_api::{
    check_cloud_api, check_cloud_api_while, check_post_api, CloudApiCheck, CloudApiCheckResult,
    PostApiCheck,
};
use cloud_checks::cloud_api_check_result;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
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
        if self.runtime_state != RuntimeState::Stopped
            || self.supervisor.is_running_or_restarting()
            || self.supervisor.is_teardown_pending()
        {
            self.append_runtime_log(format!(
                "[ui] {label} skipped: stop the runtime before reinjecting"
            ));
            return;
        }
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
            self.last_runtime_error_from_runtime = false;
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
        let xkb_layout = effective_reinject_xkb_layout(&self.applied_settings);
        self.last_runtime_error_from_runtime = false;
        self.last_runtime_error = None;
        self.last_injection_failed = false;
        self.background_task_error_revision = Some(self.runtime_error_revision);
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
                // Exit code 0 iff no failing checks (WARN is non-blocking)
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
                 --features audio-capture";
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
            "[ui] cloud API check: {} ({})",
            check.operation(),
            check.model
        ));
        let operation = check.operation();
        let (tx, rx) = mpsc::channel();
        let nemotron_probe_active = check
            .uses_in_process()
            .then(|| Arc::new(AtomicBool::new(true)));
        self.nemotron_probe_settings = nemotron_probe_active
            .is_some()
            .then(|| local_nemotron_probe_settings(&self.settings));
        if let Some(runtime_active) = nemotron_probe_active.as_ref().cloned() {
            thread::spawn(move || {
                let result = cloud_api_check_result(&check, operation, |check| {
                    check_cloud_api_while(check, runtime_active)
                });
                let _ = tx.send(result);
            });
        } else {
            thread::spawn(move || {
                let result = cloud_api_check_result(&check, operation, check_cloud_api);
                let _ = tx.send(result);
            });
        }
        self.background_task = Some(rx);
        self.background_task_label = Some("cloud API check");
        self.nemotron_probe_active = nemotron_probe_active;
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
}

// Keep the task label at this stable path; benchmark implementation and
// completion routing are owned by the child modules above.

/// Background-task label for the native benchmark run. Routed in
/// `poll_background_task` to `apply_benchmark_results`, which parses the
/// captured JSONL + the trailing `[benchmark] …` summary line into the
/// System tab's digestible model.
pub(in crate::ui) const RUN_BENCHMARK_LABEL: &str = "run benchmark";

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod window_task_tests;
