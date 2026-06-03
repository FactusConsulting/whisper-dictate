use super::*;

impl WhisperDictateApp {
    pub(super) fn runtime_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Runtime");
        ui.horizontal(|ui| {
            if ui.button("Start").clicked() {
                self.start_runtime();
            }
            if ui.button("Stop").clicked() {
                self.stop_runtime();
            }
            if ui.button("Restart").clicked() {
                self.restart_runtime();
            }
            if ui.button("Doctor").clicked() {
                self.run_doctor();
            }
            if ui
                .add_enabled(
                    self.background_task.is_none(),
                    egui::Button::new("Install/Repair"),
                )
                .clicked()
            {
                self.run_install();
            }
            if ui.button("Clear").clicked() {
                self.runtime_log.clear();
                self.runtime_log_scroll_to_bottom = true;
            }
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(self.runtime_log.clone());
            }
            ui.separator();
            ui.label(format!("Status: {}", self.runtime_state.label()));
            if let Some(label) = self.background_task_label {
                ui.label(format!("Task: {label} running"));
            }
        });

        ui.separator();
        ui.label(format!("Config: {}", self.config_path));
        ui.add_space(8.0);
        ui.label("Runtime log");
        let height = (ui.available_height() - 8.0).max(240.0);
        egui::ScrollArea::vertical()
            .id_salt("runtime_log_scroll")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .max_height(height)
            .show(ui, |ui| {
                ui.set_min_size(egui::vec2(ui.available_width(), height));
                ui.add(
                    egui::Label::new(egui::RichText::new(&self.runtime_log).monospace())
                        .selectable(true)
                        .wrap(),
                );
                let bottom = ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover());
                if self.runtime_log_scroll_to_bottom {
                    bottom.scroll_to_me(Some(egui::Align::BOTTOM));
                    self.runtime_log_scroll_to_bottom = false;
                }
            });
    }

    pub(super) fn settings_panel(&mut self, ui: &mut egui::Ui, body: fn(&mut Self, &mut egui::Ui)) {
        body(self, ui);
        ui.separator();
        let is_dirty = self.has_unsaved_settings();
        ui.horizontal(|ui| {
            let mut save_button = egui::Button::new(if is_dirty {
                egui::RichText::new("Save settings *").strong()
            } else {
                egui::RichText::new("Save settings")
            });
            if is_dirty {
                save_button = save_button.fill(ui.visuals().selection.bg_fill);
            }
            if ui
                .add_enabled(is_dirty, save_button)
                .on_hover_text("Save changed settings and any edited cloud API key.")
                .clicked()
            {
                self.save_settings();
            }
            if ui.button("Reload from disk").clicked() {
                self.reload_settings();
            }
            if is_dirty {
                ui.colored_label(ui.visuals().warn_fg_color, "Unsaved changes");
            }
            ui.label(format!("Config: {}", self.config_path));
        });
        if !self.settings_status.is_empty() {
            ui.label(&self.settings_status);
        }
    }

    pub(super) fn core_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Speech recognition");
        let backend = SttBackendMode::from_raw(&self.settings.stt_backend);
        let mut provider_id = self.current_cloud_provider().id().to_owned();
        egui::Grid::new("core_settings")
            .num_columns(2)
            .show(ui, |ui| {
                combo_help_labeled(
                    ui,
                    "Speech engine",
                    &mut self.settings.stt_backend,
                    STT_BACKEND_OPTIONS,
                    "Choose the transcription engine. Cloud STT can use either Groq or OpenAI; the saved config value is still openai for compatibility with OpenAI-compatible APIs.",
                );
                ui.end_row();
                ui.strong("Local Whisper");
                ui.label("Used only when STT backend is whisper.");
                ui.end_row();
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Whisper,
                    "Whisper model",
                    &mut self.settings.model,
                    WHISPER_MODELS,
                    "Local faster-whisper model used only with STT backend = whisper.",
                );
                ui.end_row();
                ui.strong("Local NVIDIA Parakeet");
                ui.label("Used only when STT backend is parakeet.");
                ui.end_row();
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Parakeet,
                    "Parakeet model",
                    &mut self.settings.parakeet_model,
                    PARAKEET_MODELS,
                    "Local NVIDIA NeMo Parakeet model used only with STT backend = parakeet.",
                );
                ui.end_row();
                ui.strong("Cloud STT");
                ui.label("Used only when Speech engine is Cloud STT.");
                ui.end_row();
                combo_enabled_labeled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT provider",
                    &mut provider_id,
                    CLOUD_PROVIDER_OPTIONS,
                    "Cloud transcription provider. Groq and OpenAI both use OpenAI-compatible API shapes, but each has its own URL, API key and model list.",
                );
                if let Some(provider) = CloudProvider::from_raw(&provider_id) {
                    if provider != self.current_cloud_provider() {
                        self.set_cloud_provider(provider);
                    }
                }
                ui.end_row();
                let provider = self.current_cloud_provider();
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT model",
                    &mut self.settings.stt_model,
                    provider.model_options(),
                    "Remote transcription model for the selected cloud provider. OpenAI options include gpt-4o-mini-transcribe, gpt-4o-transcribe and whisper-1.",
                );
                text_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT API URL",
                    &mut self.settings.stt_base_url,
                    "Base URL for the selected cloud transcription provider.",
                );
                text_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT timeout ms",
                    &mut self.settings.stt_timeout_ms,
                    "Network timeout for cloud transcription requests.",
                );
                password_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT API key",
                    &mut self.stt_api_key_input,
                    "Stored in the OS credential store and passed to the worker as VOICEPI_STT_API_KEY.",
                );
                ui.strong("Runtime");
                ui.label("Applies to local backends unless otherwise noted.");
                ui.end_row();
                combo_enabled(
                    ui,
                    backend != SttBackendMode::Cloud,
                    "Device",
                    &mut self.settings.device,
                    &["auto", "cuda", "cpu"],
                    "Local inference device. auto chooses CUDA when available, otherwise CPU.",
                );
                combo_enabled(
                    ui,
                    backend != SttBackendMode::Cloud,
                    "Compute type",
                    &mut self.settings.compute_type,
                    &["", "int8_float16", "float16", "bfloat16", "float32", "int8"],
                    "Local model precision/performance mode. Leave empty for backend default.",
                );
                combo_help(
                    ui,
                    "Language",
                    &mut self.settings.lang,
                    &["", "da", "en", "de", "fr", "sv", "nb", "nl", "es", "it"],
                    "Spoken language hint. Empty lets the backend autodetect when supported.",
                );
                combo_help(
                    ui,
                    "Keyboard layout",
                    &mut self.settings.xkb_layout,
                    &["", "dk", "no", "se", "de", "pt", "br", "us"],
                    "Wayland ydotool/XKB layout used for direct text injection. Use dk for Danish ae/oe/aa letters.",
                );
                text_help(
                    ui,
                    "Hotkey",
                    &mut self.settings.key,
                    "Hold-to-talk key or chord, for example ctrl_r or shift_l+ctrl_l.",
                );
                text_help(
                    ui,
                    "Quit key",
                    &mut self.settings.quit_key,
                    "Global key used to quit the worker after Quit count presses. Examples: esc, f12, q.",
                );
                text_help(
                    ui,
                    "Quit count",
                    &mut self.settings.quit_count,
                    "Number of consecutive quit-key presses required to stop the worker. 0 disables it.",
                );
                text_help(
                    ui,
                    "Quit window ms",
                    &mut self.settings.quit_window_ms,
                    "Maximum time window for consecutive quit-key presses.",
                );
            });
        if backend == SttBackendMode::Cloud {
            let provider = self.current_cloud_provider();
            ui.horizontal(|ui| {
                if ui
                    .button(format!("{} API keys", provider.label()))
                    .clicked()
                {
                    match open_url(provider.key_url()) {
                        Ok(()) => {
                            self.stt_api_key_status =
                                format!("Opened {} API keys page.", provider.label());
                        }
                        Err(err) => {
                            self.stt_api_key_status =
                                format!("Could not open {} API keys page: {err}", provider.label());
                        }
                    }
                }
                if ui
                    .button("Save API key")
                    .on_hover_text("Stores the current API key in the OS credential store without changing other settings.")
                    .clicked()
                {
                    self.save_stt_api_key_now();
                }
                if ui
                    .add_enabled(
                        self.background_task.is_none(),
                        egui::Button::new("Test cloud API"),
                    )
                    .on_hover_text("Checks the selected provider key and model from Rust without starting the Python worker.")
                    .clicked()
                {
                    self.run_cloud_api_check();
                }
                ui.label(&self.stt_api_key_status);
            });
            ui.label(
                "Paste or edit the API key above, then click Save API key or Save settings. Clear the field and save to remove the stored key.",
            );
        }
        if self.settings.stt_backend == "openai" {
            ui.label(
                "Cloud STT sends recorded audio to the configured provider. API keys are stored in the OS credential store when saved from this UI.",
            );
        }
    }

    pub(super) fn quality_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Quality");
        egui::Grid::new("quality_settings")
            .num_columns(2)
            .show(ui, |ui| {
                text_help(
                    ui,
                    "Beam size",
                    &mut self.settings.beam_size,
                    "Whisper beam search width. Higher can improve accuracy but costs more compute.",
                );
                text_help(
                    ui,
                    "Temperature ladder",
                    &mut self.settings.temperature,
                    "Comma-separated Whisper fallback temperatures, for example 0.0,0.2.",
                );
                text_help(
                    ui,
                    "Context min seconds",
                    &mut self.settings.context_min_seconds,
                    "Minimum utterance length before passing previous context/prompt hints to Whisper.",
                );
                text_help(
                    ui,
                    "Parakeet min seconds",
                    &mut self.settings.parakeet_min_seconds,
                    "Minimum captured audio length before Parakeet transcription is attempted.",
                );
                text_help(
                    ui,
                    "Release tail ms",
                    &mut self.settings.release_tail_ms,
                    "Extra audio kept after releasing the hotkey so word endings are not clipped.",
                );
                text_help(
                    ui,
                    "VAD threshold",
                    &mut self.settings.vad_threshold,
                    "Voice activity detection sensitivity. Lower is more sensitive, higher rejects more noise.",
                );
                text_help(
                    ui,
                    "VAD min silence ms",
                    &mut self.settings.vad_min_silence_ms,
                    "Silence duration used by VAD to split or end speech.",
                );
                text_help(
                    ui,
                    "Target dBFS",
                    &mut self.settings.target_dbfs,
                    "Audio normalization target loudness before transcription.",
                );
                text_help(
                    ui,
                    "Min input dBFS",
                    &mut self.settings.min_input_dbfs,
                    "Minimum raw microphone loudness accepted as speech candidate.",
                );
                text_help(
                    ui,
                    "Min SNR dB",
                    &mut self.settings.min_snr_db,
                    "Minimum signal-to-noise ratio accepted before transcription.",
                );
                checkbox_help(
                    ui,
                    "Audio ducking",
                    &mut self.settings.audio_ducking,
                    "Windows-only: temporarily lowers other app audio while recording, then restores it.",
                );
                text_help(
                    ui,
                    "Audio ducking level",
                    &mut self.settings.audio_ducking_level,
                    "Target volume for other apps while recording. 0.25 means 25%.",
                );
            });
        let show_initial_prompt_help = label_with_help(
            ui,
            "Initial prompt",
            "Optional prompt sent to Whisper for vocabulary and style hints. Keep it short; dictionary terms are capped separately.",
        );
        inline_help(ui, show_initial_prompt_help, "Optional prompt sent to Whisper for vocabulary and style hints. Keep it short; dictionary terms are capped separately.");
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.initial_prompt)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
    }

    pub(super) fn dictionary_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Dictionary");
        ui.horizontal(|ui| {
            if ui.button("Ensure file").clicked() {
                self.ensure_dictionary();
            }
            if ui.button("Open").clicked() {
                self.open_dictionary();
            }
            if ui.button("Preview").clicked() {
                self.preview_dictionary();
            }
        });
        egui::Grid::new("dictionary_settings")
            .num_columns(2)
            .show(ui, |ui| {
                text_help(
                    ui,
                    "Dictionary path",
                    &mut self.settings.dictionary,
                    "JSON dictionary used for prompt terms and deterministic replacements.",
                );
                checkbox_help(
                    ui,
                    "Dictionary enabled",
                    &mut self.settings.dictionary_enabled,
                    "Enable prompt-term injection and replacement cleanup from the dictionary.",
                );
                text_help(
                    ui,
                    "Max prompt terms",
                    &mut self.settings.dictionary_max_terms,
                    "Maximum number of dictionary terms included in the model prompt.",
                );
                text_help(
                    ui,
                    "Prompt char cap",
                    &mut self.settings.dictionary_prompt_chars,
                    "Maximum characters used by dictionary prompt terms to avoid over-steering the model.",
                );
            });
        if !self.dictionary_preview.is_empty() {
            ui.label("Prompt preview");
            ui.add(
                egui::TextEdit::multiline(&mut self.dictionary_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        }
    }

    pub(super) fn output_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Output");
        let previous_post_provider = PostProvider::from_settings(&self.settings);
        egui::Grid::new("output_settings")
            .num_columns(2)
            .show(ui, |ui| {
                combo_help(
                    ui,
                    "Inject mode",
                    &mut self.settings.inject_mode,
                    &["auto", "type", "paste", "print"],
                    "How text is inserted into the focused app. auto chooses the safest available strategy.",
                );
                combo_help(
                    ui,
                    "Format commands",
                    &mut self.settings.format_commands,
                    &["off", "en", "da", "both"],
                    "Enable spoken formatting commands such as punctuation and new lines.",
                );
                checkbox_help(
                    ui,
                    "JSON stdout",
                    &mut self.settings.inject_json,
                    "Emit structured JSON events to stdout in addition to normal logs.",
                );
                text_help(
                    ui,
                    "Metrics JSONL",
                    &mut self.settings.metrics_jsonl,
                    "Optional path for appending transcription metrics as JSONL.",
                );
                text_help(
                    ui,
                    "Command hook",
                    &mut self.settings.command_hook,
                    "Optional command run after accepted utterances for advanced automation.",
                );
                text_help(
                    ui,
                    "Command hook timeout ms",
                    &mut self.settings.command_hook_timeout_ms,
                    "Maximum time the command hook may run before it is treated as timed out.",
                );
                combo_help_labeled(
                    ui,
                    "Post processor",
                    &mut self.settings.post_processor,
                    POST_PROCESSOR_OPTIONS,
                    "Optional second text pass after speech recognition, dictionary replacements and before final injection. none disables it; ollama uses a local chat model; groq/openai send the dictated text to a cloud chat model for cleanup or rewriting.",
                );
                combo_help(
                    ui,
                    "Post mode",
                    &mut self.settings.post_mode,
                    &[
                        "raw", "clean", "prompt", "terminal", "slack", "email", "bullets",
                    ],
                    "Controls what the post processor is allowed to do. raw bypasses post-processing and does not call the model; clean fixes punctuation/casing and obvious transcription artifacts; prompt rewrites for coding agents; terminal preserves commands and paths; slack/email/bullets format for those destinations.",
                );
                match self.settings.post_processor.as_str() {
                    "groq" => combo_help(
                        ui,
                        "Post model",
                        &mut self.settings.post_model,
                        GROQ_POST_MODELS,
                        "Groq chat model used for the optional final text cleanup pass. STT Whisper models are not listed here because they transcribe audio, not text.",
                    ),
                    "openai" => combo_help(
                        ui,
                        "Post model",
                        &mut self.settings.post_model,
                        OPENAI_POST_MODELS,
                        "OpenAI chat model used for the optional final text cleanup pass.",
                    ),
                    _ => text_help(
                        ui,
                        "Post model",
                        &mut self.settings.post_model,
                        "Model name for post-processing, for example an Ollama model.",
                    ),
                }
                text_help(
                    ui,
                    "Post base URL",
                    &mut self.settings.post_base_url,
                    "Base URL for the post-processing provider. Ollama normally uses http://localhost:11434; Groq/OpenAI use OpenAI-compatible HTTPS endpoints.",
                );
                text_help(
                    ui,
                    "Post timeout ms",
                    &mut self.settings.post_timeout_ms,
                    "Maximum time allowed for post-processing.",
                );
                text_help(
                    ui,
                    "Post max input chars",
                    &mut self.settings.post_max_input_chars,
                    "Maximum transcript length sent to the post-processor.",
                );
                text_help(
                    ui,
                    "Post max output chars",
                    &mut self.settings.post_max_output_chars,
                    "Maximum accepted length of post-processed output.",
                );
                if let Some(provider) = PostProvider::from_settings(&self.settings) {
                    password_enabled(
                        ui,
                        true,
                        "Post API key",
                        &mut self.post_api_key_input,
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
                        if ui
                            .button(format!("{} API keys", provider.label()))
                            .on_hover_text("Open the provider API key page.")
                            .clicked()
                        {
                            match open_url(provider.key_url()) {
                                Ok(()) => {
                                    self.post_api_key_status =
                                        format!("Opened {} API keys page.", provider.label());
                                }
                                Err(err) => {
                                    self.post_api_key_status = format!(
                                        "Could not open {} API keys page: {err}",
                                        provider.label()
                                    );
                                }
                            }
                        }
                        ui.label(&self.post_api_key_status);
                    });
                    ui.end_row();
                }
                checkbox_help(
                    ui,
                    "Cloud redaction",
                    &mut self.settings.post_redact,
                    "Before OpenAI-compatible post-processing, replace sensitive local text with placeholders and restore it afterward when possible.",
                );
                text_help(
                    ui,
                    "Redaction terms",
                    &mut self.settings.post_redact_terms,
                    "Comma-separated names or terms to redact before cloud post-processing. Emails, phone numbers and common tokens are detected automatically.",
                );
                checkbox_help(
                    ui,
                    "History enabled",
                    &mut self.settings.history_enabled,
                    "Store local utterance history for review, copying and dictionary suggestions.",
                );
                text_help(
                    ui,
                    "History JSONL",
                    &mut self.settings.history_jsonl,
                    "Optional override path for local utterance history JSONL.",
                );
                checkbox_help(
                    ui,
                    "Local only",
                    &mut self.settings.local_only,
                    "Block network-backed STT/post-processing providers when enabled.",
                );
                checkbox_help(
                    ui,
                    "VOICEPI_DEBUG",
                    &mut self.settings.debug,
                    "Print the effective configuration at worker startup.",
                );
                checkbox_help(
                    ui,
                    "VOICEPI_STT_DEBUG",
                    &mut self.settings.stt_debug,
                    "Enable extra backend transcription diagnostics.",
                );
                text_help(
                    ui,
                    "UI text scale",
                    &mut self.settings.ui_text_scale,
                    "Scale all text in this settings UI. Use 1.0 for default, 1.15 for larger text, or 1.3 for high-DPI displays.",
                );
            });
        if PostProvider::from_settings(&self.settings) != previous_post_provider {
            self.reload_post_api_key();
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Preview history").clicked() {
                self.preview_history();
            }
            if ui.button("Open history").clicked() {
                self.open_history();
            }
            if ui.button("Preview metrics").clicked() {
                self.preview_metrics();
            }
            if ui.button("Open metrics").clicked() {
                self.open_metrics();
            }
        });
        if !self.history_preview.is_empty() {
            ui.label("History preview");
            ui.add(
                egui::TextEdit::multiline(&mut self.history_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        }
        if !self.metrics_preview.is_empty() {
            ui.label("Metrics preview");
            ui.add(
                egui::TextEdit::multiline(&mut self.metrics_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        }
    }

    pub(super) fn profiles_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");
        let show_profiles_help = label_with_help(
            ui,
            "Profiles JSON",
            "Advanced JSON profile definitions. Save persists valid JSON profiles into the config file.",
        );
        inline_help(
            ui,
            show_profiles_help,
            "Advanced JSON profile definitions. Save persists valid JSON profiles into the config file.",
        );
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.profiles_json)
                .font(egui::TextStyle::Monospace)
                .desired_rows(22)
                .desired_width(f32::INFINITY),
        );
    }
}
