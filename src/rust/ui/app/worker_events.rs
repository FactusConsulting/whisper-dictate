//! worker events responsibilities for the desktop controller.

use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn handle_worker_event(&mut self, event: &WorkerEvent) {
        if event.event == "status" {
            self.update_worker_status(event);
            if let Some(line) = worker_status_log_line(event) {
                self.append_runtime_log(line);
            }
        } else if event.event == "audio" {
            self.update_worker_audio(event);
        } else if event.event == "utterance" {
            // The dictation finished and settles into a Final card — clear the
            // live pipeline-progress card and its growing preview text.
            self.clear_pipeline_progress();
            self.last_transcript = event
                .payload
                .get("text")
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    event
                        .payload
                        .get("text_preview")
                        .and_then(serde_json::Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                        .map(str::to_owned)
                });
            self.active_profile = event
                .payload
                .get("profile")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_owned);
            self.last_target_title = event
                .payload
                .get("target_title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            self.last_target_process = event
                .payload
                .get("target_process")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            self.last_target_id = event
                .payload
                .get("target_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            self.last_inject_mode = event
                .payload
                .get("inject_mode")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|mode| !mode.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    let mode = self.settings.inject_mode.trim();
                    (!mode.is_empty()).then(|| mode.to_owned())
                });
            self.last_runtime_error = event
                .payload
                .get("inject_error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            self.last_runtime_error_from_runtime = false;
            self.last_injection_failed = self.last_runtime_error.is_some();
            if self.last_runtime_error.is_some() {
                self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            }
            if let Some(line) = worker_utterance_log_line(event) {
                self.append_runtime_log(line);
            }
        } else if event.event == "post_mode_changed" {
            // The mode-shortcut worker persisted a new post_mode in the
            // managed runtime's config .
            // Reconcile BOTH settings snapshots so the Settings page shows
            // the new value and an unrelated later Save cannot silently
            // revert the hotkey's change. Only the post_mode field is
            // touched, so pending edits to other fields survive.
            if let Some(mode) = event
                .payload
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|mode| !mode.is_empty())
            {
                // when the user already has an
                // unsaved post_mode edit, keep it as the dirty value and
                // advance only the saved baseline so the edit is not erased
                // (and not silently reverted by the next Save).
                if self.settings.post_mode == self.saved_settings.post_mode {
                    self.settings.post_mode = mode.to_owned();
                }
                self.saved_settings.post_mode = mode.to_owned();
            }
        }
    }

    pub(in crate::ui) fn update_worker_status(&mut self, event: &WorkerEvent) {
        // Surface (or clear) the prominent "device unusable" banner. The worker
        // emits `state="error"` with `reason="device_unusable"` and an actionable
        // `error` message naming the microphone the user picked that won't open on
        // any audio backend. Show it near the live-dictation header so the user
        // sees it without opening the Debug log. Only a validated recovery
        // signal clears it; ordinary recording/audio events can race with a
        // failed capture attempt and must not claim that capture is active.
        let is_device_unusable = event.state.as_deref() == Some("error")
            && worker_event_string(&event.payload, "reason").as_deref() == Some("device_unusable");
        let is_audio_recovery = matches!(
            event.state.as_deref(),
            Some("audio-fallback" | "audio-recovered")
        );
        let is_orthogonal_audio_status = is_device_unusable || is_audio_recovery;
        let recovered_device_error =
            self.apply_worker_capture_health(event, is_device_unusable, is_audio_recovery);
        if let Some(state) = event.state.as_deref() {
            self.apply_worker_status_error(
                event,
                state,
                is_device_unusable,
                recovered_device_error.as_deref(),
            );
            // Preview and audio recovery/error notifications are orthogonal:
            // none changes the active utterance/tray phase.
            if state != "preview" && !is_orthogonal_audio_status {
                self.last_worker_status_state = state.to_owned();
            }
            if !is_orthogonal_audio_status {
                self.audio_capture_opening = state == "opening";
            }
            if is_audio_recovery && self.pipeline_stage == Some("recording") {
                self.audio_capture_active = true;
            }
            // "preview" is a mid-recording, display-only signal: it must NOT
            // overwrite pipeline_stage (which would clear the live "recording"
            // spinner), so special-case it before the stage assignment and just
            // capture the growing preview text. Every other known state updates
            // the stage as before; a stage that is no longer "recording" drops
            // the stale preview text.
            if state == "preview" {
                self.pipeline_preview =
                    worker_event_string(&event.payload, "text_preview").filter(|t| !t.is_empty());
            } else if !is_orthogonal_audio_status {
                self.pipeline_stage = pipeline_stage_for_worker_state(state);
                if self.pipeline_stage != Some("recording") {
                    self.pipeline_preview = None;
                }
            }
            if let Some(ready) = worker_ready_for_state(state) {
                if ready {
                    // Worker successfully loaded and is ready — clear any crash streak.
                    self.fast_crash_count = 0;
                }
                self.worker_ready = ready;
            }
            if let Some(active) = audio_capture_active_for_worker_state(state) {
                self.audio_capture_active = active && self.device_error.is_none();
                if !self.audio_capture_active {
                    self.clear_audio_meter_readings();
                }
            }
        }
    }

    fn apply_worker_capture_health(
        &mut self,
        event: &WorkerEvent,
        is_device_unusable: bool,
        is_audio_recovery: bool,
    ) -> Option<String> {
        let recovered_device_error = is_audio_recovery
            .then(|| self.device_error.clone())
            .flatten();
        if is_device_unusable {
            self.device_error = worker_event_string(&event.payload, "error").or_else(|| {
                worker_event_string(&event.payload, "audio_device")
                    .map(|device| format!("Microphone {device} could not be opened."))
            });
            self.audio_capture_active = false;
            self.audio_capture_opening = false;
            self.clear_audio_meter_readings();
        }
        if let Some(audio_device) = worker_event_string(&event.payload, "audio_device") {
            // Recovery events are emitted only after bounded capture health
            // validation, so they are the authoritative signal that the
            // previously unavailable microphone path is working again.
            if is_audio_recovery {
                self.device_error = None;
            }
            self.active_audio_device = audio_device;
        }
        recovered_device_error
    }

    fn apply_worker_status_error(
        &mut self,
        event: &WorkerEvent,
        state: &str,
        is_device_unusable: bool,
        recovered_device_error: Option<&str>,
    ) {
        if let Some(recovered_error) = recovered_device_error {
            if self.last_runtime_error.as_deref() == Some(recovered_error) {
                self.last_runtime_error_from_runtime = false;
                self.last_runtime_error = None;
                self.last_injection_failed = false;
            }
        }
        if matches!(
            state,
            "opening" | "recording" | "transcribing" | "post-processing" | "injecting"
        ) {
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = None;
            self.last_injection_failed = false;
        }
        if state == "profile" {
            self.active_profile = worker_event_string(&event.payload, "active_profile")
                .filter(|profile| !profile.trim().is_empty());
            self.active_target_title =
                worker_event_string(&event.payload, "target_title").unwrap_or_default();
            self.active_target_process =
                worker_event_string(&event.payload, "target_process").unwrap_or_default();
            self.active_target_id =
                worker_event_string(&event.payload, "target_id").unwrap_or_default();
        }
        match state {
            "ready" if !self.last_injection_failed => {
                self.last_runtime_error_from_runtime = false;
                self.last_runtime_error = None;
            }
            "error" | "failed" | "capture_lost" if !is_device_unusable => {
                self.last_injection_failed = false;
                let error = worker_event_string(&event.payload, "error")
                    .or_else(|| worker_event_string(&event.payload, "reason"))
                    .or_else(|| Some(state.replace('_', " ")));
                self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
                self.last_runtime_error_from_runtime = false;
                self.last_runtime_error = error;
            }
            "no_text" => {
                if let Some(error) = worker_event_string(&event.payload, "error") {
                    self.last_injection_failed = true;
                    self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
                    self.last_runtime_error_from_runtime = false;
                    self.last_runtime_error = Some(error);
                }
            }
            _ => {}
        }
    }

    /// The runtime state to show in the UI: the OS process spawns almost
    /// instantly (`Running`), but a local model can take a while to load, so we
    /// keep displaying `Starting` until the worker reports it is ready to receive
    /// speech. Control logic (Start/Stop enabling, audio meter) still uses the
    /// raw `runtime_state`.
    pub(in crate::ui) fn display_runtime_state(&self) -> RuntimeState {
        match self.runtime_state {
            RuntimeState::Running if !self.worker_ready => RuntimeState::Starting,
            other => other,
        }
    }

    pub(in crate::ui) fn update_worker_audio(&mut self, event: &WorkerEvent) {
        if let Some(audio_device) = worker_event_string(&event.payload, "audio_device") {
            self.active_audio_device = audio_device;
        }
        if let Some(level) = worker_event_f32(&event.payload, "level") {
            self.audio_meter_level = level.clamp(0.0, 1.0);
        }
        if let Some(raw_dbfs) = worker_event_f32(&event.payload, "raw_dbfs") {
            self.audio_meter_raw_dbfs = Some(raw_dbfs);
        }
        if let Some(peak) = worker_event_f32(&event.payload, "peak") {
            self.audio_meter_peak = Some(peak);
        }
        if let Some(state) = event.state.as_deref() {
            self.audio_capture_opening = state == "opening";
            if let Some(active) = audio_capture_active_for_worker_state(state) {
                self.audio_capture_active = active && self.device_error.is_none();
                if !self.audio_capture_active {
                    self.clear_audio_meter_readings();
                }
            }
        } else {
            // No state field: treat as active only when the worker is actually
            // running so a stale/malformed audio event can't linger the meter
            // after capture has stopped.
            if self.runtime_state == RuntimeState::Running && self.device_error.is_none() {
                self.audio_capture_active = true;
            } else {
                self.audio_capture_active = false;
                self.clear_audio_meter_readings();
            }
        }
    }
}
