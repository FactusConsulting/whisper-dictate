//! Persistence-side app state: saving/reloading the config plus the cloud and
//! post-processing provider normalization and API-key storage flows.

use super::*;
use anyhow::Result;

/// Canonicalize legacy/case-insensitive Nemotron model ids before a settings
/// save. The picker stores the full ids, while older config files may contain
/// the short aliases; treating a recognized alias as an unknown model would
/// silently switch an English NIM deployment to the multilingual profile.
fn canonical_nemotron_model(model: &str) -> Option<&'static str> {
    if crate::dictate::backends::cloud_transcribe::is_nemotron_english_model(model) {
        Some(NEMOTRON_ENGLISH_STT_MODEL)
    } else if crate::dictate::backends::cloud_transcribe::is_nemotron_model_alias(model) {
        Some(NEMOTRON_MULTI_STT_MODEL)
    } else {
        None
    }
}

impl WhisperDictateApp {
    pub(in crate::ui) fn save_settings(&mut self) {
        let preserve_stt_model_clear = self.stt_model_is_explicitly_cleared();
        self.normalize_cloud_provider_settings();
        self.normalize_postprocessor_settings();
        if let Err(err) = serde_json::from_str::<serde_json::Value>(&self.settings.profiles_json) {
            if preserve_stt_model_clear {
                self.settings.stt_model.clear();
            }
            self.settings_status = format!("Profiles JSON is invalid: {err}");
            return;
        }
        let mut explicit_nulls = self
            .explicit_nullable_clears
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if preserve_stt_model_clear && !explicit_nulls.contains(&"stt_model") {
            // Hosted providers need a concrete model while the typed settings
            // snapshot is validated. Keep that normalized value for
            // validation, but serialize the user's persisted null intent.
            explicit_nulls.push("stt_model");
        }
        match config::save_settings_with_explicit_nulls(&self.settings, &explicit_nulls) {
            Ok(path) => {
                let restart_keys = config::restart_required_keys_with_explicit_nulls(
                    &self.saved_settings,
                    &self.settings,
                    &explicit_nulls,
                );
                let enabling_local_only =
                    !self.saved_settings.local_only && self.settings.local_only;
                let prior_stt_key = self.saved_stt_api_key_input.clone();
                let prior_post_key = self.saved_post_api_key_input.clone();
                // Re-poll the update check immediately when its settings changed
                // (e.g. enabling "Include release candidates"), instead of
                // waiting out the current poll interval.
                if update_check_settings_changed(&self.saved_settings, &self.settings) {
                    self.last_update_check = None;
                }
                let key_message = self.save_stt_api_key_if_changed();
                let post_key_message = self.save_post_api_key_if_changed();
                let credentials_changed = prior_stt_key != self.saved_stt_api_key_input
                    || prior_post_key != self.saved_post_api_key_input;
                if preserve_stt_model_clear {
                    self.settings.stt_model.clear();
                }
                self.saved_settings = self.settings.clone();
                self.explicit_nullable_clears.clear();
                self.settings_status = format!("Saved settings: {}", path.display());
                self.append_runtime_log(format!("[ui] settings saved: {}", path.display()));
                if enabling_local_only {
                    let cancelled = self.whisper_model_downloads.cancel_all();
                    if cancelled > 0 {
                        let message = format!(
                            "Cancelling {cancelled} model download(s) for local-only mode."
                        );
                        self.settings_status.push_str(" | ");
                        self.settings_status.push_str(&message);
                        self.append_runtime_log(format!("[ui] {message}"));
                    }
                }
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
                if self.supervisor.is_running_or_restarting()
                    && (!restart_keys.is_empty() || credentials_changed)
                {
                    let mut reasons = restart_keys;
                    if credentials_changed {
                        reasons.push("api_credentials");
                    }
                    self.append_runtime_log(format!(
                        "[ui] restart required after settings change: {}",
                        reasons.join(", ")
                    ));
                    self.restart_runtime();
                }
            }
            Err(err) => {
                if preserve_stt_model_clear {
                    self.settings.stt_model.clear();
                }
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
            || !self.explicit_nullable_clears.is_empty()
            || self.stt_api_key_input != self.saved_stt_api_key_input
            || self.post_api_key_input != self.saved_post_api_key_input
    }

    pub(in crate::ui) fn reload_settings(&mut self) {
        match config::load_settings() {
            Ok(settings) => {
                self.explicit_nullable_clears.clear();
                self.saved_settings = settings.clone();
                self.runtime_log_view = LogViewMode::from_raw(&settings.ui_log_view);
                self.settings = settings;
                self.reload_stt_api_key();
                self.reload_post_api_key();
                self.settings_status = "Reloaded config".to_owned();
                self.append_runtime_log(format!("[ui] settings loaded: {}", self.config_path));
            }
            Err(err) => {
                self.settings_status = format!("Reload failed: {err}");
            }
        }
    }

    pub(in crate::ui) fn record_nullable_selection(&mut self, key: &str, value: &str) {
        if value.trim().is_empty() {
            self.explicit_nullable_clears.insert(key.to_owned());
        } else {
            self.explicit_nullable_clears.remove(key);
        }
    }

    pub(in crate::ui) fn record_nullable_text_edit(
        &mut self,
        key: &str,
        before: &str,
        after: &str,
    ) {
        if before != after {
            self.record_nullable_selection(key, after);
        }
    }

    pub(in crate::ui) fn current_cloud_provider(&self) -> CloudProvider {
        CloudProvider::from_raw(&self.settings.stt_provider)
            .unwrap_or_else(|| CloudProvider::from_settings(&self.settings))
    }

    pub(in crate::ui) fn set_cloud_provider(&mut self, provider: CloudProvider) {
        let prior = self.current_cloud_provider();
        self.settings.stt_backend = "openai".to_owned();
        self.apply_cloud_provider_defaults(provider);
        if provider == CloudProvider::Nemotron && prior != CloudProvider::Nemotron {
            self.settings.stt_base_url = provider.base_url().to_owned();
            self.settings.stt_model = provider.default_model().to_owned();
        }
        let model = self.settings.stt_model.clone();
        self.record_nullable_selection("stt_model", &model);
        self.reload_stt_api_key();
    }

    /// Keep the English Nemotron profile and the shared language picker in a
    /// valid combination. The profile has no language-ID capability, so an
    /// Auto/blank (or non-English) hint would make the next request fail at
    /// the service boundary. Selecting the profile therefore makes the
    /// compatible English choice explicit and records it as a normal pending
    /// settings edit for the user to save.
    pub(in crate::ui) fn normalize_nemotron_profile_language(&mut self) -> Option<String> {
        if self.current_cloud_provider() != CloudProvider::Nemotron
            || !crate::dictate::backends::cloud_transcribe::is_nemotron_english_model(
                &self.settings.stt_model,
            )
            || crate::dictate::backends::cloud_transcribe::is_english_language_hint(
                &self.settings.lang,
            )
        {
            return None;
        }
        self.settings.lang = "en".to_owned();
        self.record_nullable_selection("lang", "en");
        Some(
            "English Nemotron profile selected; Language set to English. Choose Multilingual / Auto for automatic language detection."
                .to_owned(),
        )
    }

    fn normalize_cloud_provider_settings(&mut self) {
        if self.settings.stt_backend == "openai" {
            let provider = self.current_cloud_provider();
            self.apply_cloud_provider_defaults(provider);
            if provider == CloudProvider::Nemotron {
                self.settings.stt_base_url =
                    crate::cloud_api::canonical_nemotron_endpoint(&self.settings.stt_base_url);
            }
        }
    }

    fn stt_model_is_explicitly_cleared(&self) -> bool {
        if !self.settings.stt_model.trim().is_empty() {
            return false;
        }
        if self.explicit_nullable_clears.contains("stt_model") {
            return true;
        }
        config::load_raw_config()
            .ok()
            .and_then(|value| value.as_object().cloned())
            .and_then(|object| object.get("stt_model").cloned())
            .is_some_and(|value| {
                value.is_null() || value.as_str().is_some_and(|model| model.trim().is_empty())
            })
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
        if provider == CloudProvider::Nemotron {
            let url = self.settings.stt_base_url.trim();
            if url.is_empty()
                || url == OPENAI_STT_BASE_URL
                || url == GROQ_STT_BASE_URL
                || url == CUSTOM_STT_BASE_URL
                || url.eq_ignore_ascii_case(NEMOTRON_LEGACY_HTTP_STT_BASE_URL)
                || url.eq_ignore_ascii_case("http://localhost:9000")
            {
                self.settings.stt_base_url = provider.base_url().to_owned();
            }
            self.settings.stt_base_url =
                crate::cloud_api::canonical_nemotron_endpoint(&self.settings.stt_base_url);
        } else {
            self.settings.stt_base_url = provider.base_url().to_owned();
        }
        if provider == CloudProvider::Nemotron {
            if let Some(model) = canonical_nemotron_model(&self.settings.stt_model) {
                self.settings.stt_model = model.to_owned();
            } else if !provider
                .model_options()
                .contains(&self.settings.stt_model.as_str())
            {
                self.settings.stt_model = provider.default_model().to_owned();
            }
        } else if !provider
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
        // Do not write a credential when the coupled profile/language
        // settings would make the provider-settings save fail. The key must
        // not get ahead of the configuration it belongs to.
        if let Err(err) = self.settings.validate_nemotron_profile_language() {
            let message = format!("Could not save {} API key: {err}", provider.label());
            self.stt_api_key_status = message.clone();
            self.append_runtime_log(format!("[ERROR] cloud API key save blocked: {message}"));
            return;
        }
        let prior_saved_key = self.saved_stt_api_key_input.clone();
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
        self.restart_after_credential_change(
            "cloud STT",
            prior_saved_key != self.saved_stt_api_key_input,
        );
    }

    pub(in crate::ui) fn persist_cloud_provider_selection(
        &mut self,
    ) -> Result<Option<std::path::PathBuf>> {
        let provider = self.current_cloud_provider();
        if provider == CloudProvider::Nemotron {
            self.settings.stt_base_url =
                crate::cloud_api::canonical_nemotron_endpoint(&self.settings.stt_base_url);
        }
        let mut saved = self.saved_settings.clone();
        saved.stt_backend = "openai".to_owned();
        saved.stt_provider = provider.id().to_owned();
        saved.stt_base_url = if matches!(provider, CloudProvider::Custom | CloudProvider::Nemotron)
        {
            if provider == CloudProvider::Nemotron {
                crate::cloud_api::canonical_nemotron_endpoint(&self.settings.stt_base_url)
            } else {
                self.settings.stt_base_url.clone()
            }
        } else {
            provider.base_url().to_owned()
        };
        saved.stt_model = self.settings.stt_model.clone();
        // The English Nemotron profile normalizes the shared language picker
        // to `en`. Copy it with that provider/model pair so the dedicated Save
        // API key flow cannot validate or persist a stale Auto value from the
        // previous provider snapshot. For every other provider, leave `lang`
        // in the on-disk snapshot alone: Save API key is not Save settings and
        // must not silently consume an unrelated language edit.
        let persist_language = provider == CloudProvider::Nemotron
            && crate::dictate::backends::cloud_transcribe::is_nemotron_english_model(
                &saved.stt_model,
            );
        if persist_language {
            saved.lang = self.settings.lang.clone();
        }

        if saved == self.saved_settings {
            return Ok(None);
        }

        let path = config::save_settings(&saved)?;
        self.saved_settings.stt_backend = saved.stt_backend;
        self.saved_settings.stt_provider = saved.stt_provider;
        self.saved_settings.stt_base_url = saved.stt_base_url;
        self.saved_settings.stt_model = saved.stt_model;
        if persist_language {
            self.saved_settings.lang = saved.lang;
        }
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
        let prior_saved_key = self.saved_post_api_key_input.clone();
        self.post_api_key_status = self.save_post_api_key_message();
        self.restart_after_credential_change(
            "cloud post-processing",
            prior_saved_key != self.saved_post_api_key_input,
        );
    }

    pub(in crate::ui) fn restart_after_credential_change(&mut self, kind: &str, changed: bool) {
        if !changed {
            return;
        }
        if crate::diag::debug_enabled() {
            self.append_runtime_log(format!(
                "[ui/debug] {kind} credential snapshot changed; runtime_running={}",
                self.supervisor.is_running()
            ));
        }
        if self.supervisor.is_running() {
            self.append_runtime_log(format!(
                "[ui] restart required after {kind} credential change"
            ));
            self.restart_runtime_after_credential_change();
        }
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
