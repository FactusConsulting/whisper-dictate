//! runtime lifecycle responsibilities for the desktop controller.

use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn start_runtime(&mut self) {
        if self.background_task_label == Some(tasks::RUN_BENCHMARK_LABEL) {
            let message = "Cannot start the runtime while a benchmark is running.";
            self.settings_status = message.to_owned();
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = Some(message.to_owned());
            self.append_runtime_log(format!("[ui] start blocked: {message}"));
            return;
        }
        if self.nemotron_probe_active.is_some() {
            let message =
                "Cannot start the runtime while the local Nemotron model test is running.";
            self.settings_status = message.to_owned();
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = Some(message.to_owned());
            self.append_runtime_log(format!("[ui] start blocked: {message}"));
            return;
        }
        if self.transcript_action_running() {
            let message = "Cannot start the runtime while a transcript action is running.";
            self.settings_status = message.to_owned();
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = Some(message.to_owned());
            self.append_runtime_log(format!("[ui] start blocked: {message}"));
            return;
        }
        if !self.validate_nemotron_profile_language_for_runtime("start") {
            return;
        }
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            let message = self.cloud_stt_missing_api_key_message();
            self.settings_status = message.clone();
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = Some(message.clone());
            self.append_runtime_log(format!("[ui] start blocked: {message}"));
            return;
        }
        if let Some(warning) = self.runtime_whisper_model_warning() {
            self.settings_status = warning.clone();
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = Some(warning.clone());
            self.append_runtime_log(format!("[ui] start blocked: {warning}"));
            return;
        }
        self.cancel_hotkey_verification("normal runtime start requested");
        self.installed_hotkey = None;
        self.worker_ready = false;
        self.last_injection_failed = false;
        self.last_runtime_error_from_runtime = false;
        self.last_runtime_error = None;
        self.active_target_title.clear();
        self.active_target_process.clear();
        self.active_target_id.clear();
        self.clear_audio_meter_and_device();
        let command = self.runtime_worker_command();
        self.append_runtime_log(format!("[ui] starting: {}", command.display()));
        if let Err(err) = self.supervisor.start(command) {
            self.pending_runtime_settings = None;
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = Some(err.to_string());
            self.append_runtime_log(format!("[ui] start failed: {err}"));
        } else {
            self.pending_runtime_settings = Some(self.settings.clone());
            self.worker_start_time = Some(std::time::Instant::now());
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn stop_runtime(&mut self) {
        self.pending_runtime_settings = None;
        self.worker_ready = false;
        self.last_injection_failed = false;
        self.last_runtime_error_from_runtime = false;
        self.last_runtime_error = None;
        self.installed_hotkey = None;
        self.active_target_title.clear();
        self.active_target_process.clear();
        self.active_target_id.clear();
        self.clear_audio_meter_and_device();
        self.clear_pipeline_progress();
        self.append_runtime_log("[ui] stopping runtime");
        if let Err(err) = self.supervisor.stop() {
            self.append_runtime_log(format!("[ui] stop failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn restart_runtime(&mut self) {
        self.restart_runtime_inner(false);
    }

    pub(in crate::ui) fn restart_runtime_after_credential_change(&mut self) {
        self.restart_runtime_inner(true);
    }

    /// The Nemotron profile/language mismatch message for the snapshot that
    /// would actually launch, or `None` when it is fine. Side-effect-free
    /// (reads state, raises no banner, logs nothing) so it can be reused
    /// both by the Start-button-facing gate below AND by the launch-time
    /// auto-start precheck (`ui/autostart.rs`), which needs the same
    /// diagnostic WITHOUT the banner a manual Start would raise.
    ///
    /// Start/restart launches `runtime_worker_command`, whose base values
    /// come from the persisted config rather than the still-editable form.
    /// Validate that exact snapshot so an unsaved picker edit cannot block
    /// a runtime that would launch with the last saved, valid settings.
    pub(in crate::ui) fn nemotron_profile_language_error(&self) -> Option<String> {
        let effective_settings = self.runtime_worker_command().runtime.settings().clone();
        if effective_settings.stt_backend != "openai" {
            return None;
        }
        effective_settings
            .validate_nemotron_profile_language()
            .err()
            .map(|err| err.to_string())
    }

    pub(in crate::ui) fn validate_nemotron_profile_language_for_runtime(
        &mut self,
        operation: &str,
    ) -> bool {
        let Some(message) = self.nemotron_profile_language_error() else {
            return true;
        };
        self.settings_status = message.clone();
        self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
        self.last_runtime_error_from_runtime = false;
        self.last_runtime_error = Some(message.clone());
        self.append_runtime_log(format!("[ui] {operation} blocked: {message}"));
        false
    }

    fn restart_runtime_inner(&mut self, credential_restart: bool) {
        if self.transcript_action_running() {
            let message = "Cannot restart the runtime while a transcript action is running.";
            self.settings_status = message.to_owned();
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = Some(message.to_owned());
            self.append_runtime_log(format!("[ui] restart blocked: {message}"));
            return;
        }
        if !self.validate_nemotron_profile_language_for_runtime("restart") {
            return;
        }
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            let message = self.cloud_stt_missing_api_key_message();
            self.settings_status = message.clone();
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = Some(message.clone());
            self.append_runtime_log(format!("[ui] restart blocked: {message}"));
            return;
        }
        if let Some(warning) = self.runtime_whisper_model_warning() {
            let status = if self.local_only_change_pending() {
                format!("Restart pending: local-only change is not active. {warning}")
            } else if credential_restart {
                format!("Restart pending: credential change is not active. {warning}")
            } else {
                warning.clone()
            };
            self.settings_status = status.clone();
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = Some(status.clone());
            self.append_runtime_log(format!("[ui] restart blocked: {status}"));
            return;
        }
        let command = self.runtime_worker_command();
        self.installed_hotkey = None;
        self.worker_ready = false;
        self.last_injection_failed = false;
        self.last_runtime_error_from_runtime = false;
        self.last_runtime_error = None;
        self.active_target_title.clear();
        self.active_target_process.clear();
        self.active_target_id.clear();
        self.clear_audio_meter_and_device();
        self.clear_pipeline_progress();
        self.append_runtime_log(format!("[ui] restarting: {}", command.display()));
        if let Err(err) = self.supervisor.restart(command) {
            self.pending_runtime_settings = None;
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            self.last_runtime_error_from_runtime = false;
            self.last_runtime_error = Some(err.to_string());
            self.append_runtime_log(format!("[ui] restart failed: {err}"));
        } else {
            self.pending_runtime_settings = Some(self.settings.clone());
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn worker_command(&self) -> WorkerCommand {
        let mut command = default_worker_command();
        command
            .runtime
            .set_stt_provider(self.settings.stt_provider.clone());
        if let Some(xkb_layout) = effective_xkb_layout(&self.settings) {
            command
                .runtime
                .set(XKB_LAYOUT_ENV, xkb_layout)
                .expect("xkb layout is a schema-backed runtime setting");
        }
        if self.settings.stt_backend == "openai" {
            let key = self.stt_api_key_input.trim();
            if !key.is_empty() {
                command
                    .runtime
                    .set(STT_API_KEY_ENV, key)
                    .expect("STT credential is a scoped runtime value");
            }
        }
        // Track provenance of the pushed post key so
        // `stamp_post_api_key_endpoint_marker` can bind the marker to the
        // ENDPOINT the underlying credential was resolved for -- not the
        // configured post endpoint blindly. Codex P1 round-2 #1
        // (`PRRT_kwDOSfNjQs6UXpn-` cmt 3665199618): a Groq-STT + OpenAI-post
        // setup used to stamp the OpenAI marker for a mirrored Groq STT
        // key, which the revalidation check then APPROVED for OpenAI --
        // a cross-provider leak of the STT key.
        let mut post_key_provenance = crate::runtime::cloud_api_keys::PostKeyProvenance::None;
        if matches!(self.settings.post_processor.as_str(), "openai" | "groq") {
            let post_key = self.post_api_key_input.trim();
            let stt_key = self.stt_api_key_input.trim();
            // Codex P1 round-3 (`PRRT_kwDOSfNjQs6UZdNL` cmt 3665509647):
            // `load_post_api_key_state` (see `ui/api_keys.rs:204-209`)
            // populates `post_api_key_input` from the `VOICEPI_STT_API_KEY`
            // env fallback when no post-specific credential is saved. The
            // field is then NON-EMPTY, so the old "empty post -> SttMirror,
            // else PostSpecific" rule stamped the OpenAI post endpoint for a
            // key that was actually the Groq STT key -- another
            // cross-provider approval. The fix: treat provenance as SttMirror
            // whenever the post field's VALUE equals the STT field's value.
            // Reason it's safe both ways: if the user pasted the same key
            // into both fields intentionally, that key IS the STT key (they
            // configured it as such), so binding the marker to the STT
            // endpoint is the correct + strictest classification. If the
            // values genuinely differ, the post field holds a post-specific
            // key and PostSpecific applies.
            let (key, provenance) = if post_key.is_empty() {
                (
                    stt_key,
                    crate::runtime::cloud_api_keys::PostKeyProvenance::SttMirror,
                )
            } else if !stt_key.is_empty() && post_key == stt_key {
                (
                    post_key,
                    crate::runtime::cloud_api_keys::PostKeyProvenance::SttMirror,
                )
            } else {
                (
                    post_key,
                    crate::runtime::cloud_api_keys::PostKeyProvenance::PostSpecific,
                )
            };
            if !key.is_empty() {
                // Codex P2 round-4 (`PRRT_kwDOSfNjQs6UZxN2` cmt 3665701506):
                // if the value we are about to push equals the ambient
                // `VOICEPI_POST_API_KEY` (i.e. it was loaded from the parent
                // env via `load_post_api_key_state`'s env fallback -- see
                // `ui/api_keys.rs:204-209`), the child inherits it from the
                // parent env automatically (`std::process::Command::envs`
                // extends rather than clears). Skipping the push AND the
                // marker preserves the documented "explicit env keys own
                // their resolution" compatibility contract -- an
                // env-provided key intentionally travels marker-free, and
                // a launcher marker stamped for the current endpoint would
                // otherwise reject the user's own key after any live
                // provider/profile change, with a restart producing the
                // same rejection because the same env value would load and
                // stamp again.
                if command.runtime_credential_is_ambient(POST_API_KEY_ENV, key) {
                    // Caller-owned via the boundary snapshot; keep provenance
                    // unset so endpoint ownership remains with the caller.
                } else {
                    command
                        .runtime
                        .set(POST_API_KEY_ENV, key)
                        .expect("post credential is a scoped runtime value");
                    post_key_provenance = provenance;
                }
            }
        }
        // Codex P1 #666 #1 (`PRRT_kwDOSfNjQs6UXpn-`): the UI Start button
        // built the worker command separately from terminal credential
        // resolution, so `VOICEPI_POST_API_KEY_ENDPOINT` was
        // never stamped for the primary Windows tray path -- the exact
        // Groq-to-OpenAI/custom live-change leak from #642 remained
        // exploitable through the shipping default flow. This shim stamps
        // the marker with the same saved-credential rules, so
        // `require_endpoint_matches_marker` in the worker enforces the
        // endpoint check regardless of which entry point launched it.
        crate::runtime::cloud_api_keys::stamp_post_api_key_endpoint_marker(
            &mut command,
            post_key_provenance,
            &self.settings.post_processor,
            &self.settings.post_base_url,
            &self.settings.stt_backend,
            &self.settings.stt_base_url,
        );
        command
    }

    pub(in crate::ui) fn runtime_worker_command(&self) -> WorkerCommand {
        if crate::diag::trace_enabled() {
            crate::diag::log!("[ui/trace] building isolated native runtime settings snapshot");
        }
        self.worker_command()
    }

    pub(in crate::ui) fn transcript_action_running(&self) -> bool {
        self.background_task.is_some()
            && matches!(
                self.background_task_label,
                Some(crate::ui::tasks::REINJECT_LAST_LABEL | crate::ui::tasks::RETRY_LAST_LABEL)
            )
    }
}
