use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn post_processing_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Post-processing");
        let previous_post_provider = PostProvider::from_settings(&self.settings);
        // Everything below the "Post processor" selector only applies once a
        // processor is chosen — grey it out and lock it while post is disabled.
        let post_enabled = self.settings.post_processor != "none";
        let language = self.settings.ui_language.clone();
        settings_grid("post_processing_settings")
            .show(ui, |ui| {
                combo_help_labeled(
                    ui,
                    "Post processor",
                    &mut self.settings.post_processor,
                    POST_PROCESSOR_OPTIONS,
                    "Optional second text pass after speech recognition, dictionary replacements and before final injection. none disables it; ollama uses a local chat model; groq/openai send the dictated text to a cloud chat model for cleanup or rewriting.",
                );
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
                self.post_model_field(ui, post_enabled);
                text_enabled(
                    ui,
                    post_enabled,
                    "Post base URL",
                    &mut self.settings.post_base_url,
                    "Base URL for the post-processing provider. Ollama normally uses http://localhost:11434; Groq/OpenAI use OpenAI-compatible HTTPS endpoints.",
                );
                numeric_enabled(
                    ui,
                    &language,
                    post_enabled,
                    "post_timeout_ms",
                    "Post timeout ms",
                    &mut self.settings.post_timeout_ms,
                    "Base (floor) time for post-processing. The effective timeout scales with the transcript length (longer text gets more time) up to a 30s ceiling, then falls back to the un-cleaned text.",
                );
                numeric_enabled(
                    ui,
                    &language,
                    post_enabled,
                    "post_max_input_chars",
                    "Post max input chars",
                    &mut self.settings.post_max_input_chars,
                    "Maximum transcript length sent to the post-processor.",
                );
                numeric_enabled(
                    ui,
                    &language,
                    post_enabled,
                    "post_max_output_chars",
                    "Post max output chars",
                    &mut self.settings.post_max_output_chars,
                    "Maximum accepted length of post-processed output.",
                );
                if let Some(provider) = PostProvider::from_settings(&self.settings) {
                    self.post_api_key_section(ui, provider);
                }
                checkbox_enabled(
                    ui,
                    post_enabled,
                    "Cloud redaction",
                    &mut self.settings.post_redact,
                    "Before OpenAI-compatible post-processing, replace sensitive local text with placeholders and restore it afterward when possible.",
                );
                text_enabled(
                    ui,
                    post_enabled,
                    "Redaction terms",
                    &mut self.settings.post_redact_terms,
                    "Comma-separated names or terms to redact before cloud post-processing. Emails, phone numbers and common tokens are detected automatically.",
                );
            });
        if PostProvider::from_settings(&self.settings) != previous_post_provider {
            self.reload_post_api_key();
        }
    }

    fn post_model_field(&mut self, ui: &mut egui::Ui, enabled: bool) {
        match self.settings.post_processor.as_str() {
            "groq" => combo_enabled_labeled(
                ui,
                enabled,
                "Post model",
                &mut self.settings.post_model,
                GROQ_POST_MODELS,
                "Groq chat model used for the optional final text cleanup pass. The list labels show the recommended Danish cleanup default, faster alternatives, reasoning models and preview models. STT Whisper models are not listed here because they transcribe audio, not text.",
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
