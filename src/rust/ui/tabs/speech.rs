use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn core_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        let language = self.settings.ui_language.clone();
        ui.heading("Speech recognition");
        let backend = SttBackendMode::from_raw(&self.settings.stt_backend);

        // Speech engine selector stays above all groups — it is the switch that
        // decides which group is active, so it must be visible unconditionally.
        settings_grid("speech_engine_selector").show(ui, |ui| {
            combo_help_labeled(
                ui,
                "Speech engine",
                &mut self.settings.stt_backend,
                STT_BACKEND_OPTIONS,
                "Choose the transcription engine. Cloud STT can use either Groq or OpenAI; the saved config value is still openai for compatibility with OpenAI-compatible APIs.",
            );
        });

        ui.add_space(6.0);

        // --- Whisper group -----------------------------------------------
        scope_group(
            ui,
            palette,
            ui_text(&language, UiTextKey::SpeechGroupWhisper),
            "speech_whisper",
            |ui| {
                let gpu_total_mb = self.gpu_total_mb;
                combo_model_vram(
                    ui,
                    backend == SttBackendMode::Whisper,
                    "Whisper model",
                    &mut self.settings.model,
                    WHISPER_MODELS,
                    whisper_model_hint,
                    gpu_total_mb,
                    "Larger models are more accurate but slower and use more VRAM. On a CUDA GPU, \
                     models that don't fit your VRAM are greyed out; on CPU every model runs (large \
                     ones just slower). The ~MB figure is the approximate VRAM at the int8_float16 \
                     GPU default. Used only when STT backend is whisper.",
                );
            },
        );

        ui.add_space(6.0);

        // --- Parakeet group ----------------------------------------------
        scope_group(
            ui,
            palette,
            ui_text(&language, UiTextKey::SpeechGroupParakeet),
            "speech_parakeet",
            |ui| {
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Parakeet,
                    "Parakeet model",
                    &mut self.settings.parakeet_model,
                    PARAKEET_MODELS,
                    "Local NVIDIA NeMo Parakeet model used only with STT backend = parakeet.",
                );
            },
        );

        ui.add_space(6.0);

        // --- Online / Cloud STT group ------------------------------------
        let mut provider_id = self.current_cloud_provider().id().to_owned();
        scope_group(
            ui,
            palette,
            ui_text(&language, UiTextKey::SpeechGroupOnline),
            "speech_online",
            |ui| {
                combo_enabled_labeled_short(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT provider",
                    &mut provider_id,
                    CLOUD_PROVIDER_OPTIONS,
                    "Cloud transcription provider. Groq and OpenAI both use OpenAI-compatible API shapes, but each has its own URL, API key and model list.",
                );
                // Commit the provider change immediately (not after the closure)
                // so the dependent model/URL/key widgets AND the Save/Test action
                // buttons below all operate on the just-selected provider in the
                // same frame — `provider_id` is a local String, so this borrows
                // self only transiently between widget calls.
                if let Some(selected) = CloudProvider::from_raw(&provider_id) {
                    if selected != self.current_cloud_provider() {
                        self.set_cloud_provider(selected);
                    }
                }
                let provider = self.current_cloud_provider();
                if provider == CloudProvider::Custom {
                    text_enabled(
                        ui,
                        backend == SttBackendMode::Cloud,
                        "Cloud STT model",
                        &mut self.settings.stt_model,
                        "Model id your self-hosted OpenAI-compatible server expects, for example Systran/faster-whisper-large-v3.",
                    );
                } else {
                    combo_enabled(
                        ui,
                        backend == SttBackendMode::Cloud,
                        "Cloud STT model",
                        &mut self.settings.stt_model,
                        provider.model_options(),
                        "Remote transcription model for the selected cloud provider. OpenAI options include gpt-4o-mini-transcribe, gpt-4o-transcribe and whisper-1.",
                    );
                }
                text_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT API URL",
                    &mut self.settings.stt_base_url,
                    "Base URL for the selected cloud transcription provider.",
                );
                numeric_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "stt_timeout_ms",
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
            },
        );

        ui.add_space(6.0);

        // --- General group -----------------------------------------------
        // Device and Compute type are passed to BOTH WhisperModel and
        // ParakeetModel (see vp_transcribe.py load_stt_model), so they span
        // both local backends and belong here rather than in either
        // engine-specific group.
        scope_group(
            ui,
            palette,
            ui_text(&language, UiTextKey::SpeechGroupGeneral),
            "speech_general",
            |ui| {
                combo_enabled_short(
                    ui,
                    backend != SttBackendMode::Cloud,
                    "Device",
                    &mut self.settings.device,
                    &["auto", "cuda", "cpu"],
                    "Local inference device. auto chooses CUDA when available, otherwise CPU. Used by Whisper and Parakeet.",
                );
                combo_enabled_labeled(
                    ui,
                    backend != SttBackendMode::Cloud,
                    "Compute type",
                    &mut self.settings.compute_type,
                    &[
                        ("", "Auto — best precision for your device (recommended)"),
                        ("float32", "float32 — most accurate, slowest, most memory"),
                        ("bfloat16", "bfloat16 — near-float32 accuracy (newer GPUs)"),
                        ("float16", "float16 — high accuracy, ~half the VRAM (GPU)"),
                        (
                            "int8_float16",
                            "int8_float16 — fast, low VRAM (good GPU default)",
                        ),
                        (
                            "int8",
                            "int8 — fastest, least memory, slight accuracy loss (CPU)",
                        ),
                    ],
                    "Numeric precision the local Whisper model runs at. Higher precision is more \
                     accurate but slower and uses more VRAM/RAM; lower is faster and lighter for a \
                     small accuracy cost. Auto picks a sensible default per device (GPU vs CPU). \
                     Parakeet ignores this — it always uses its own precision.",
                );
                self.microphone_settings(ui);
                combo_help_labeled_short(
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
                    combo_help_labeled_short(
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
                checkbox_help(
                    ui,
                    "Toggle mode",
                    &mut self.settings.toggle_mode,
                    "Toggle mode: press the hotkey to start recording, press again to stop and transcribe — instead of holding it.",
                );
                text_help(
                    ui,
                    "Quit key",
                    &mut self.settings.quit_key,
                    "Global key used to quit the worker after Quit count presses. Examples: esc, f12, q.",
                );
                text_help_short(
                    ui,
                    "Quit count",
                    &mut self.settings.quit_count,
                    "Number of consecutive quit-key presses required to stop the worker. 0 disables it.",
                );
                text_help_short(
                    ui,
                    "Quit window ms",
                    &mut self.settings.quit_window_ms,
                    "Maximum time window for consecutive quit-key presses.",
                );
            },
        );
    }

    /// The Microphone picker: a combo over "(System default)" + the refreshed
    /// device list (with the saved value preserved even if absent), plus a
    /// "Refresh devices" button that runs the worker's `--list-audio-devices`.
    /// Visible on every speech backend — capture is backend-independent.
    fn microphone_settings(&mut self, ui: &mut egui::Ui) {
        const MIC_HELP: &str = "Input device for recording. (System default) uses the OS default \
            microphone. Otherwise the worker matches your saved value against device names \
            (case-insensitive substring) or treats a number as a device index. Refresh devices \
            asks the worker to list the inputs it can see.";
        let options = self.microphone_options();
        combo_help_dynamic(
            ui,
            "Microphone",
            &mut self.settings.audio_device,
            &options,
            MIC_HELP,
        );

        ui.label("");
        if ui
            .add_enabled(
                self.background_task.is_none(),
                egui::Button::new("Refresh devices"),
            )
            .on_hover_text(
                "Run the worker to list available microphones. The result populates the picker; \
                 it does not load a model or start dictation.",
            )
            .clicked()
        {
            self.run_list_audio_devices();
        }
        ui.end_row();
    }

    /// Build the Microphone combo entries: "(System default)" → "" first, then
    /// the refreshed device names, and finally the saved value as an extra entry
    /// when it is non-empty and not already listed (so a custom/offline device
    /// is never silently dropped).
    pub(in crate::ui) fn microphone_options(&self) -> Vec<(String, String)> {
        let mut options: Vec<(String, String)> =
            vec![(String::new(), "(System default)".to_owned())];
        for name in &self.audio_device_options {
            options.push((name.clone(), name.clone()));
        }
        let saved = self.settings.audio_device.trim();
        if !saved.is_empty() && !options.iter().any(|(value, _)| value == saved) {
            options.push((saved.to_owned(), format!("{saved} (saved)")));
        }
        options
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
