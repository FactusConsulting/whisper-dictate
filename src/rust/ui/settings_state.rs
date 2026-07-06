//! Persistence-side app state: saving/reloading the config plus the cloud and
//! post-processing provider normalization and API-key storage flows.

use super::*;
use anyhow::Result;

impl WhisperDictateApp {
    pub(in crate::ui) fn save_settings(&mut self) {
        self.normalize_cloud_provider_settings();
        self.normalize_postprocessor_settings();
        if let Err(err) = serde_json::from_str::<serde_json::Value>(&self.settings.profiles_json) {
            self.settings_status = format!("Profiles JSON is invalid: {err}");
            return;
        }
        match config::save_settings(&self.settings) {
            Ok(path) => {
                let restart_keys =
                    config::restart_required_keys(&self.saved_settings, &self.settings);
                // Re-poll the update check immediately when its settings changed
                // (e.g. enabling "Include release candidates"), instead of
                // waiting out the current poll interval.
                if update_check_settings_changed(&self.saved_settings, &self.settings) {
                    self.last_update_check = None;
                }
                // Codex P2 (settings_schema.json:323, PR #440) — honour
                // the schema's `"live": true` claim by reinstalling
                // (or clearing) the process-global auto-mute controller
                // as soon as the user saves a change to this key. The
                // controller was previously (re)installed only on
                // worker start, so toggling this in Settings silently
                // had no effect until the next manual restart. Uses
                // `install_from_settings` so an env override still wins.
                if self.saved_settings.mute_output_while_recording
                    != self.settings.mute_output_while_recording
                {
                    // Codex P2 (session.rs:130, PR #440) — Save always
                    // means "the user made this the on-disk value", so
                    // pass Some(...) so env can never override.
                    crate::output_mute::session::install_from_settings(Some(
                        self.settings.mute_output_while_recording,
                    ));
                    self.append_runtime_log(format!(
                        "[ui] mute-output-while-recording toggled -> {}",
                        self.settings.mute_output_while_recording,
                    ));
                }
                let key_message = self.save_stt_api_key_if_changed();
                let post_key_message = self.save_post_api_key_if_changed();
                self.saved_settings = self.settings.clone();
                self.settings_status = format!("Saved settings: {}", path.display());
                self.append_runtime_log(format!("[ui] settings saved: {}", path.display()));
                if let Some(message) = key_message {
                    self.settings_status.push_str(" | ");
                    self.settings_status.push_str(&message);
                    self.append_runtime_log(format!("[ui] cloud API key save: {message}"));
                }
                if let Some(message) = post_key_message {
                    self.settings_status.push_str(" | ");
                    self.settings_status.push_str(&message);
                    self.append_runtime_log(format!("[ui] post API key save: {message}"));
                }
                if self.supervisor.is_running() && !restart_keys.is_empty() {
                    self.append_runtime_log(format!(
                        "[ui] restart required after settings change: {}",
                        restart_keys.join(", ")
                    ));
                    self.restart_runtime();
                }
            }
            Err(err) => {
                self.settings_status = format!("Save failed: {err}");
            }
        }
    }

    /// Apply and persist the runtime log-view preference. The toolbar toggle
    /// applies instantly *and* writes just this view setting, so switching the
    /// log view doesn't leave the whole settings form looking "unsaved" — and it
    /// never commits the user's other pending edits (those stay in `settings`
    /// until an explicit Save). `saved_settings` is the on-disk snapshot, so
    /// persisting a copy of it with the new view keeps the dirty check clean.
    pub(in crate::ui) fn set_log_view(&mut self, mode: LogViewMode) {
        self.runtime_log_view = mode;
        self.settings.ui_log_view = mode.id().to_owned();
        self.runtime_log_scroll_to_bottom = true;
        self.saved_settings.ui_log_view = mode.id().to_owned();
        if let Err(err) = config::save_settings(&self.saved_settings) {
            self.append_runtime_log(format!("[ui] could not persist log view: {err}"));
        }
    }

    pub(in crate::ui) fn has_unsaved_settings(&self) -> bool {
        self.settings != self.saved_settings
            || self.stt_api_key_input != self.saved_stt_api_key_input
            || self.post_api_key_input != self.saved_post_api_key_input
    }

    pub(in crate::ui) fn reload_settings(&mut self) {
        match config::load_settings() {
            Ok(mut settings) => {
                // Re-apply the same metrics_jsonl prefill used at app construction
                // so the field never goes blank after "Reload config".
                if settings.metrics_jsonl.trim().is_empty() {
                    settings.metrics_jsonl = tabs::default_metrics_jsonl_path(&self.config_path);
                }
                self.saved_settings = settings.clone();
                self.runtime_log_view = LogViewMode::from_raw(&settings.ui_log_view);
                self.settings = settings;
                self.reload_stt_api_key();
                self.reload_post_api_key();
                // Codex P2 (settings_state.rs:38, PR #440) — honour the
                // schema's `"live": true` claim on this key by
                // reinstalling (or clearing) the process-global
                // auto-mute controller when Reload config picks up a
                // change. Without this, editing config.json externally
                // and clicking Reload left the UI showing the new value
                // while the runtime kept the old mute state until a
                // Save toggle or a restart. `install_from_settings`
                // resolves env fallback the same way as start-up.
                crate::output_mute::session::install_from_settings(Some(
                    self.settings.mute_output_while_recording,
                ));
                self.settings_status = "Reloaded config".to_owned();
                self.append_runtime_log(format!("[ui] settings loaded: {}", self.config_path));
            }
            Err(err) => {
                self.settings_status = format!("Reload failed: {err}");
            }
        }
    }

    pub(in crate::ui) fn current_cloud_provider(&self) -> CloudProvider {
        CloudProvider::from_raw(&self.settings.stt_provider)
            .unwrap_or_else(|| CloudProvider::from_settings(&self.settings))
    }

    pub(in crate::ui) fn set_cloud_provider(&mut self, provider: CloudProvider) {
        self.settings.stt_backend = "openai".to_owned();
        self.apply_cloud_provider_defaults(provider);
        self.reload_stt_api_key();
    }

    fn normalize_cloud_provider_settings(&mut self) {
        if self.settings.stt_backend == "openai" {
            let provider = self.current_cloud_provider();
            self.apply_cloud_provider_defaults(provider);
        }
    }

    fn apply_cloud_provider_defaults(&mut self, provider: CloudProvider) {
        self.settings.stt_provider = provider.id().to_owned();
        if provider == CloudProvider::Custom {
            // A self-hosted endpoint is user-managed: never overwrite the base URL
            // or model. Only seed a localhost starting point when switching in
            // from a hosted provider (or from nothing).
            let url = self.settings.stt_base_url.trim();
            if url.is_empty() || url == OPENAI_STT_BASE_URL || url == GROQ_STT_BASE_URL {
                self.settings.stt_base_url = CUSTOM_STT_BASE_URL.to_owned();
            }
            return;
        }
        self.settings.stt_base_url = provider.base_url().to_owned();
        if !provider
            .model_options()
            .contains(&self.settings.stt_model.as_str())
        {
            self.settings.stt_model = provider.default_model().to_owned();
        }
    }

    fn normalize_postprocessor_settings(&mut self) {
        match self.settings.post_processor.as_str() {
            "groq" => {
                self.settings.post_base_url = GROQ_STT_BASE_URL.to_owned();
                if !labeled_options_contain(GROQ_POST_MODELS, &self.settings.post_model) {
                    self.settings.post_model = GROQ_POST_MODEL.to_owned();
                }
            }
            "openai" => {
                self.settings.post_base_url = OPENAI_STT_BASE_URL.to_owned();
                if !OPENAI_POST_MODELS.contains(&self.settings.post_model.as_str()) {
                    self.settings.post_model = OPENAI_POST_MODEL.to_owned();
                }
            }
            "ollama"
                if self.settings.post_base_url.trim().is_empty()
                    || self.settings.post_base_url == GROQ_STT_BASE_URL
                    || self.settings.post_base_url == OPENAI_STT_BASE_URL =>
            {
                self.settings.post_base_url = "http://localhost:11434".to_owned();
            }
            _ => {}
        }
    }

    pub(in crate::ui) fn reload_stt_api_key(&mut self) {
        let provider = self.current_cloud_provider();
        match load_stt_api_key_state(provider) {
            Ok((key, saved_key, status)) => {
                self.stt_api_key_input = key;
                self.saved_stt_api_key_input = saved_key;
                self.stt_api_key_status = status;
            }
            Err(err) => {
                self.stt_api_key_input.clear();
                self.saved_stt_api_key_input.clear();
                self.stt_api_key_status = format!("Could not load API key: {err}");
            }
        }
    }

    pub(in crate::ui) fn reload_post_api_key(&mut self) {
        match load_post_api_key_state(PostProvider::from_settings(&self.settings)) {
            Ok((key, saved_key, status)) => {
                self.post_api_key_input = key;
                self.saved_post_api_key_input = saved_key;
                self.post_api_key_status = status;
            }
            Err(err) => {
                self.post_api_key_input.clear();
                self.saved_post_api_key_input.clear();
                self.post_api_key_status = format!("Could not load post-processing API key: {err}");
            }
        }
    }

    fn save_stt_api_key_if_changed(&mut self) -> Option<String> {
        if self.settings.stt_backend != "openai" {
            return None;
        }
        if self.stt_api_key_input == self.saved_stt_api_key_input {
            return None;
        }
        let provider = self.current_cloud_provider();
        let message = match save_stt_api_key(provider, self.stt_api_key_input.trim()) {
            Ok(report) => {
                self.saved_stt_api_key_input = self.stt_api_key_input.clone();
                if self.stt_api_key_input.trim().is_empty() {
                    format!("Cleared saved {} API key.", provider.label())
                } else {
                    format!(
                        "Saved {} API key in {}.",
                        provider.label(),
                        report.status_label()
                    )
                }
            }
            Err(err) => {
                format!("Could not save {} API key: {err}", provider.label())
            }
        };
        self.stt_api_key_status = message.clone();
        Some(message)
    }

    pub(in crate::ui) fn save_stt_api_key_now(&mut self) {
        if self.settings.stt_backend != "openai" {
            self.stt_api_key_status =
                "API keys are only used when STT backend is Cloud STT.".to_owned();
            return;
        }
        let provider = self.current_cloud_provider();
        self.apply_cloud_provider_defaults(provider);
        let mut key_log_details = None;
        let key_message = match save_stt_api_key(provider, self.stt_api_key_input.trim()) {
            Ok(report) => {
                key_log_details = Some(report.log_details());
                self.saved_stt_api_key_input = self.stt_api_key_input.clone();
                if self.stt_api_key_input.trim().is_empty() {
                    format!(
                        "Cleared saved {} API key. {}",
                        provider.label(),
                        report.status_label()
                    )
                } else {
                    format!(
                        "Saved {} API key in {}.",
                        provider.label(),
                        report.status_label()
                    )
                }
            }
            Err(err) => {
                format!("Could not save {} API key: {err}", provider.label())
            }
        };
        match self.persist_cloud_provider_selection() {
            Ok(Some(path)) => {
                self.stt_api_key_status =
                    format!("{key_message} Saved provider settings: {}", path.display());
                self.append_runtime_log(format!(
                    "[ui] cloud API key save: {key_message}; {}; provider_settings={}",
                    key_log_details
                        .as_deref()
                        .unwrap_or("no secret save details"),
                    path.display()
                ));
            }
            Ok(None) => {
                self.stt_api_key_status = key_message;
                self.append_runtime_log(format!(
                    "[ui] cloud API key save: {}; {}",
                    self.stt_api_key_status,
                    key_log_details
                        .as_deref()
                        .unwrap_or("no secret save details")
                ));
            }
            Err(err) => {
                self.stt_api_key_status =
                    format!("{key_message} Provider settings save failed: {err}");
                self.append_runtime_log(format!(
                    "[ERROR] cloud API key save: {}; provider settings save failed: {err}",
                    key_message
                ));
            }
        }
    }

    pub(in crate::ui) fn persist_cloud_provider_selection(
        &mut self,
    ) -> Result<Option<std::path::PathBuf>> {
        let provider = self.current_cloud_provider();
        let mut saved = self.saved_settings.clone();
        saved.stt_backend = "openai".to_owned();
        saved.stt_provider = provider.id().to_owned();
        saved.stt_base_url = provider.base_url().to_owned();
        saved.stt_model = self.settings.stt_model.clone();

        if saved == self.saved_settings {
            return Ok(None);
        }

        let path = config::save_settings(&saved)?;
        self.saved_settings.stt_backend = saved.stt_backend;
        self.saved_settings.stt_provider = saved.stt_provider;
        self.saved_settings.stt_base_url = saved.stt_base_url;
        self.saved_settings.stt_model = saved.stt_model;
        Ok(Some(path))
    }

    fn save_post_api_key_if_changed(&mut self) -> Option<String> {
        if self.post_api_key_input == self.saved_post_api_key_input {
            return None;
        }
        if PostProvider::from_settings(&self.settings).is_none()
            && self.post_api_key_input.is_empty()
        {
            return None;
        }
        let message = self.save_post_api_key_message();
        self.post_api_key_status = message.clone();
        Some(message)
    }

    pub(in crate::ui) fn save_post_api_key_now(&mut self) {
        self.post_api_key_status = self.save_post_api_key_message();
    }

    fn save_post_api_key_message(&mut self) -> String {
        let Some(provider) = PostProvider::from_settings(&self.settings) else {
            return "Post API keys are only used when Post processor is Groq or OpenAI.".to_owned();
        };
        match save_post_api_key(provider, self.post_api_key_input.trim()) {
            Ok(report) => {
                let log_details = report.log_details();
                self.saved_post_api_key_input = self.post_api_key_input.clone();
                let message = if self.post_api_key_input.trim().is_empty() {
                    format!("Cleared saved {} API key.", provider.label())
                } else {
                    format!(
                        "Saved {} API key in {}.",
                        provider.label(),
                        report.status_label()
                    )
                };
                self.append_runtime_log(format!(
                    "[ui] post API key save: {}; {}",
                    message, log_details
                ));
                message
            }
            Err(err) => format!("Could not save {} API key: {err}", provider.label()),
        }
    }
}
