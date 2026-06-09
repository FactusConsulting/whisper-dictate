use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn core_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        ui.heading("Speech recognition");
        let backend = SttBackendMode::from_raw(&self.settings.stt_backend);
        let mut provider_id = self.current_cloud_provider().id().to_owned();
        settings_grid("core_settings")
            .show(ui, |ui| {
                combo_help_labeled(
                    ui,
                    "Speech engine",
                    &mut self.settings.stt_backend,
                    STT_BACKEND_OPTIONS,
                    "Choose the transcription engine. Cloud STT can use either Groq or OpenAI; the saved config value is still openai for compatibility with OpenAI-compatible APIs.",
                );
                ui.end_row();
                section_label(ui, "Local Whisper", palette);
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
                section_label(ui, "Local NVIDIA Parakeet", palette);
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
                section_label(ui, "Cloud STT", palette);
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
                    &mut self.stt_api_key_reveal_until,
                    "Stored in the OS credential store and passed to the worker as VOICEPI_STT_API_KEY.",
                );
                if backend == SttBackendMode::Cloud {
                    self.cloud_stt_key_section(ui, provider);
                }
                section_label(ui, "Local runtime", palette);
                ui.label("Used only by local Whisper and Parakeet.");
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
                section_label(ui, "Dictation controls", palette);
                ui.label("Applies to local and cloud speech engines.");
                ui.end_row();
                combo_help_labeled(
                    ui,
                    "Language",
                    &mut self.settings.lang,
                    &[
                        ("", "Auto"),
                        ("da", "Danish"),
                        ("en", "English"),
                        ("de", "German"),
                        ("fr", "French"),
                        ("sv", "Swedish"),
                        ("nb", "Norwegian"),
                        ("nl", "Dutch"),
                        ("es", "Spanish"),
                        ("it", "Italian"),
                    ],
                    "Spoken language hint. Auto lets the backend autodetect when supported.",
                );
                if !cfg!(windows) {
                    combo_help_labeled(
                        ui,
                        "Linux keyboard layout",
                        &mut self.settings.xkb_layout,
                        &[
                            ("", "Auto"),
                            ("dk", "Danish"),
                            ("no", "Norwegian"),
                            ("se", "Swedish"),
                            ("de", "German"),
                            ("pt", "Portuguese"),
                            ("br", "Brazilian"),
                            ("us", "US English"),
                        ],
                        "Wayland ydotool/XKB layout used for direct text injection on Linux. Auto detects GNOME layout when possible.",
                    );
                }
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
    }

    fn cloud_stt_key_section(&mut self, ui: &mut egui::Ui, provider: CloudProvider) {
        ui.label("");
        ui.horizontal(|ui| {
            if ui
                .button("Save API key")
                .on_hover_text(
                    "Stores the current API key in the platform credential store and remembers the selected cloud provider. Clear the field and save to remove it.",
                )
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
            if provider == CloudProvider::Groq
                && ui
                    .link("Open Groq API keys")
                    .on_hover_text("Open the Groq API key page.")
                    .clicked()
            {
                self.open_groq_keys_page_stt(provider);
            }
        });
        ui.end_row();
        ui.label("");
        let key_help = if self.saved_stt_api_key_input.trim().is_empty() {
            "Paste an API key, then save it. Cloud STT sends recorded audio to the configured provider."
        } else {
            "Saved key loaded. Edit and save to replace it, or clear the field and save to remove it."
        };
        ui.label(key_help).on_hover_text(
            "API keys are stored in the platform credential store when possible. If that fails, the app reports the fallback location in the runtime log.",
        );
        ui.end_row();
    }

    fn open_groq_keys_page_stt(&mut self, provider: CloudProvider) {
        match open_url(provider.key_url()) {
            Ok(()) => {
                self.stt_api_key_status = "Opened Groq API keys page.".to_owned();
            }
            Err(err) => {
                self.stt_api_key_status = format!("Could not open Groq API keys page: {err}");
            }
        }
    }
}
