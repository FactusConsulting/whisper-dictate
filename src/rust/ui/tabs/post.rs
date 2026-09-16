pub(super) const GROQ_POST_MODEL_HELP: &str = "Groq chat model used for the optional final text cleanup pass. Choose the recommended fast cleanup model or the heavier highest-quality model. STT Whisper models are not listed here because they transcribe audio, not text.";
use super::*;

impl WhisperDictateApp {
    /// The Post-processing tab. Shown in BOTH settings modes since #895's
    /// sibling change: Simple keeps the two essential rows (`post_processor`
    /// and `post_mode`, the only two marked `ui_simple` in
    /// `settings_schema.json`) plus the cloud API-key block, which a cloud
    /// processor cannot work without. Every other row is gated row-by-row on
    /// its own `setting_visible`, exactly like the Speech and System tabs, so
    /// a later schema flag flip needs no edit here.
    pub(in crate::ui) fn post_processing_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Post-processing");
        let mode = SettingsMode::from_raw(&self.settings.ui_settings_mode);
        let previous_post_provider = PostProvider::from_settings(&self.settings);
        // Everything below the "Post processor" selector only applies once a
        // processor is chosen — grey it out and lock it while post is disabled.
        let post_enabled = self.settings.post_processor != "none";
        let language = self.settings.ui_language.clone();
        settings_grid("post_processing_settings")
            .show(ui, |ui| {
                if setting_visible(mode, "post_processor") {
                    combo_help_labeled(
                        ui,
                        "Post processor",
                        &mut self.settings.post_processor,
                        POST_PROCESSOR_OPTIONS,
                        "Optional second text pass after speech recognition, dictionary replacements and before final injection. none disables it; ollama uses a local chat model; groq/openai send the dictated text to a cloud chat model for cleanup or rewriting.",
                    );
                }
                if setting_visible(mode, "post_mode") {
                    combo_enabled_short(
                        ui,
                        post_enabled,
                        "Post mode",
                        &mut self.settings.post_mode,
                        &[
                            "raw", "clean", "prompt", "terminal", "slack", "email", "bullets",
                        ],
                        "Controls what the post processor is allowed to do. raw bypasses post-processing and does not call the model; clean fixes punctuation/casing and obvious transcription artifacts; prompt rewrites for coding agents; terminal preserves commands and paths; slack/email/bullets format for those destinations.",
                    );
                }
                if setting_visible(mode, "post_model") {
                    self.post_model_field(ui, post_enabled);
                }
                let post_redact_terms_before = self.settings.post_redact_terms.clone();
                if setting_visible(mode, "post_base_url") {
                    text_enabled(
                        ui,
                        post_enabled,
                        "Post base URL",
                        &mut self.settings.post_base_url,
                        "Base URL for the post-processing provider. Ollama normally uses http://localhost:11434; Groq/OpenAI use OpenAI-compatible HTTPS endpoints.",
                    );
                }
                if setting_visible(mode, "post_timeout_ms") {
                    numeric_enabled(
                        ui,
                        &language,
                        post_enabled,
                        "post_timeout_ms",
                        "Post timeout ms",
                        &mut self.settings.post_timeout_ms,
                        "Base (floor) time for post-processing. The effective timeout scales with the transcript length (longer text gets more time) up to a 30-second ceiling, then falls back to the uncleaned text.",
                    );
                }
                if setting_visible(mode, "post_max_input_chars") {
                    numeric_enabled(
                        ui,
                        &language,
                        post_enabled,
                        "post_max_input_chars",
                        "Post max input chars",
                        &mut self.settings.post_max_input_chars,
                        "Maximum transcript length sent to the post-processor.",
                    );
                }
                if setting_visible(mode, "post_max_output_chars") {
                    numeric_enabled(
                        ui,
                        &language,
                        post_enabled,
                        "post_max_output_chars",
                        "Post max output chars",
                        &mut self.settings.post_max_output_chars,
                        "Maximum accepted length of post-processed output.",
                    );
                }
                if setting_visible(mode, "post_api_key") {
                    if let Some(provider) = PostProvider::from_settings(&self.settings) {
                        self.post_api_key_section(ui, provider);
                    }
                }
                if setting_visible(mode, "post_redact") {
                    checkbox_enabled(
                        ui,
                        post_enabled,
                        "Cloud redaction",
                        &mut self.settings.post_redact,
                        "Before OpenAI-compatible post-processing, replace sensitive local text with placeholders and restore it afterward when possible.",
                    );
                }
                if setting_visible(mode, "post_redact_terms") {
                    text_enabled(
                        ui,
                        post_enabled,
                        "Redaction terms",
                        &mut self.settings.post_redact_terms,
                        "Comma-separated names or terms to redact before cloud post-processing. Emails, phone numbers and common tokens are detected automatically.",
                    );
                }
                let post_redact_terms_after = self.settings.post_redact_terms.clone();
                self.record_nullable_text_edit(
                    "post_redact_terms",
                    &post_redact_terms_before,
                    &post_redact_terms_after,
                );
            });
        self.apply_post_provider_change(previous_post_provider);
    }

    /// Normalize the post-processing endpoint/model and reload the cached
    /// API key the instant `post_processor` changes to a different provider
    /// — mirrors `set_cloud_provider`'s instant-apply pattern for the STT
    /// side (the #888 P1 fix), rather than leaving the endpoint stale until
    /// an explicit Save.
    ///
    /// This matters ESPECIALLY in Simple mode (Opus review P1, 2026-09-16):
    /// `post_base_url` is an Advanced-only row, hidden in Simple, so before
    /// this fix it stayed pointed at the OLD provider's endpoint until Save
    /// — but "Save post API key" and "Test post API" (both reachable from
    /// the Simple-visible key block) fire immediately. Changing Post
    /// processor from openai to groq (the two rows Simple DOES show) and
    /// clicking "Test post API" sent the newly pasted Groq key straight to
    /// `api.openai.com`. `normalize_postprocessor_settings` forces
    /// `post_base_url`/`post_model` onto the new provider's own values for
    /// groq/openai (a hosted provider has exactly one valid endpoint, so
    /// there is nothing user-managed to preserve there — unlike `ollama`,
    /// whose self-hosted URL the same function already leaves untouched
    /// unless it still holds an old hosted default), so the request the key
    /// actually goes to always matches the provider the picker shows.
    pub(in crate::ui) fn apply_post_provider_change(
        &mut self,
        previous_post_provider: Option<PostProvider>,
    ) {
        if PostProvider::from_settings(&self.settings) == previous_post_provider {
            return;
        }
        self.normalize_postprocessor_settings();
        self.reload_post_api_key();
    }

    fn post_model_field(&mut self, ui: &mut egui::Ui, enabled: bool) {
        match self.settings.post_processor.as_str() {
            "groq" => combo_enabled_labeled(
                ui,
                enabled,
                "Post model",
                &mut self.settings.post_model,
                GROQ_POST_MODELS,
                GROQ_POST_MODEL_HELP,
            ),
            "openai" => combo_enabled(
                ui,
                enabled,
                "Post model",
                &mut self.settings.post_model,
                OPENAI_POST_MODELS,
                "OpenAI chat model used for the optional final text cleanup pass.",
            ),
            _ => text_enabled(
                ui,
                enabled,
                "Post model",
                &mut self.settings.post_model,
                "Model name for post-processing, for example an Ollama model.",
            ),
        }
    }

    fn post_api_key_section(&mut self, ui: &mut egui::Ui, provider: PostProvider) {
        password_enabled(
            ui,
            true,
            "Post API key",
            &mut self.post_api_key_input,
            &mut self.post_api_key_reveal_until,
            "Optional separate API key for cloud post-processing. Stored in the OS credential store as VOICEPI_POST_API_KEY. If empty, the worker falls back to the Cloud STT API key when available.",
        );
        ui.label("");
        ui.horizontal(|ui| {
            if ui
                .button("Save post API key")
                .on_hover_text("Stores only the post-processing API key in the OS credential store.")
                .clicked()
            {
                self.save_post_api_key_now();
            }
            if ui
                .button("Test post API")
                .on_hover_text("Sends a tiny chat-completions request to the selected post-processing provider and model.")
                .clicked()
            {
                self.run_post_api_check();
            }
            self.test_post_api_indicator(ui);
            if provider == PostProvider::Groq
                && ui
                    .link("Open Groq API keys")
                    .on_hover_text("Open the Groq API key page.")
                    .clicked()
            {
                self.open_groq_keys_page(provider);
            }
        });
        ui.end_row();
        ui.label("");
        let key_help = if self.saved_post_api_key_input.trim().is_empty() {
            "Optional separate post-processing key. Leave empty to reuse the Cloud STT key when available."
        } else {
            "Saved post-processing key loaded. Edit and save to replace it, or clear the field and save to remove it."
        };
        ui.label(key_help).on_hover_text(
            "Post-processing API keys are stored in the platform credential store when possible. If that fails, the app reports the fallback location in the runtime log.",
        );
        ui.end_row();
    }

    /// Render the inline ✓/✗/testing indicator next to "Test post API" from the
    /// stored `post_api_key_status` and whether the post-API check is in flight.
    /// Delegates to the shared `render_api_check_indicator` shell.
    fn test_post_api_indicator(&self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        let in_flight = self.background_task_label == Some("post API check");
        render_api_check_indicator(ui, &self.post_api_key_status, in_flight, palette);
    }

    fn open_groq_keys_page(&mut self, provider: PostProvider) {
        match open_url(provider.key_url()) {
            Ok(()) => {
                self.post_api_key_status = "Opened Groq API keys page.".to_owned();
            }
            Err(err) => {
                self.post_api_key_status = format!("Could not open Groq API keys page: {err}");
            }
        }
    }
}
