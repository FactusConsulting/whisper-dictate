use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn core_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        let language = self.settings.ui_language.clone();
        // Simple/Advanced mode (Issue #334). Advanced-only rows below are
        // gated with `row_visible(mode, false)` so Simple mode shows only
        // the fields a first-time user needs. Simple-marked rows (mic,
        // hotkey, backend/model, language, API key) are always visible.
        let mode = SettingsMode::from_raw(&self.settings.settings_mode);
        let show_advanced = row_visible(mode, false);
        ui.heading("Speech recognition");
        let backend = SttBackendMode::from_raw(&self.settings.stt_backend);

        // Speech engine selector stays above all groups — it is the switch that
        // decides which group is active, so it must be visible unconditionally.
        // Wrapped in a frameless inset that matches scope_group's horizontal
        // inner margin so the dropdown value column aligns with the fields
        // inside the boxed groups below (approach B — inset without a box).
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(SCOPE_GROUP_INNER_MARGIN_H, 0))
            .show(ui, |ui| {
                settings_grid("speech_engine_selector").show(ui, |ui| {
                    combo_help_labeled(
                        ui,
                        "Speech engine",
                        &mut self.settings.stt_backend,
                        STT_BACKEND_OPTIONS,
                        "Choose the transcription engine. Cloud STT can use either Groq or OpenAI; the saved config value is still openai for compatibility with OpenAI-compatible APIs.",
                    );
                });
            });

        ui.add_space(6.0);

        // --- Whisper group -----------------------------------------------
        // The download section needs `&mut self` so it can't live inside the
        // `scope_group`'s `FnOnce(&mut egui::Ui)` (which borrows self for the
        // closure capture). Render the existing model picker via the group
        // first, then drop the closure scope before invoking the section.
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
        // Wave 7-B: in-app GGML model downloader. Sits next to the model
        // picker so users discover it where they already pick a model.
        // Hidden in Simple mode — a fresh user picks a model; managing
        // downloads is a power-user surface.
        if show_advanced {
            self.whisper_model_download_section(ui);
        }

        ui.add_space(6.0);

        // Wave 8 of #348: the Parakeet group has been removed together with
        // the backend. The picker above no longer offers "Local NVIDIA
        // Parakeet"; saved configs with `stt_backend = "parakeet"` are
        // migrated to whisper at load time.

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
                // Base URL + timeout are Advanced-only: a Simple-mode user
                // stays on the provider's default endpoint and timeout.
                if show_advanced {
                    text_enabled(
                        ui,
                        backend == SttBackendMode::Cloud,
                        "Cloud STT API URL",
                        &mut self.settings.stt_base_url,
                        "Base URL for the selected cloud transcription provider.",
                    );
                    numeric_enabled(
                        ui,
                        &language,
                        backend == SttBackendMode::Cloud,
                        "stt_timeout_ms",
                        "Cloud STT timeout ms",
                        &mut self.settings.stt_timeout_ms,
                        "Network timeout for cloud transcription requests.",
                    );
                }
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
        // Device and Compute type are passed to WhisperModel (see
        // vp_transcribe.py load_stt_model) and belong here rather than in
        // either engine-specific group.
        scope_group(
            ui,
            palette,
            ui_text(&language, UiTextKey::SpeechGroupGeneral),
            "speech_general",
            |ui| {
                // Device + compute type are Advanced-only: the local
                // Whisper "auto"/int8_float16 defaults handle the vast
                // majority of setups; a new user shouldn't have to reason
                // about precision.
                if show_advanced {
                    combo_enabled_short(
                        ui,
                        backend != SttBackendMode::Cloud,
                        "Device",
                        &mut self.settings.device,
                        &["auto", "cuda", "cpu"],
                        "Local inference device. auto chooses CUDA when available, otherwise CPU. Used by the local Whisper backend.",
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
                         small accuracy cost. Auto picks a sensible default per device (GPU vs CPU).",
                    );
                }
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
                // Linux xkb layout is a niche override — hidden in Simple.
                if show_advanced && !cfg!(windows) {
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
                hotkey_help(
                    ui,
                    &language,
                    palette,
                    "Hotkey",
                    &mut self.settings.key,
                    "Hold-to-talk key or chord, for example ctrl_r or shift_l+ctrl_l. \
                     Join keys with '+'. Tokens are key names (modifiers and named keys).",
                );
                // Toggle mode + the quit-key chord are Advanced-only —
                // most users are fine with push-to-talk and don't need a
                // global multi-press exit shortcut on top of it.
                if show_advanced {
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
                    numeric_help(
                        ui,
                        &language,
                        "quit_count",
                        "Quit count",
                        &mut self.settings.quit_count,
                        "Number of consecutive quit-key presses required to stop the worker. 0 disables it.",
                    );
                    numeric_help(
                        ui,
                        &language,
                        "quit_window_ms",
                        "Quit window ms",
                        &mut self.settings.quit_window_ms,
                        "Maximum time window for consecutive quit-key presses.",
                    );
                }
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
        // Snapshot the selected device so we can detect a combo change below: the
        // device-test result belongs to the previously-selected device, so a
        // different selection must clear the stale ✓/✗ rather than leave it
        // pinned next to the new device.
        let device_before = self.settings.audio_device.clone();
        combo_help_dynamic(
            ui,
            "Microphone",
            &mut self.settings.audio_device,
            &options,
            MIC_HELP,
        );
        self.clear_device_test_result_if_device_changed(&device_before);

        let language = self.settings.ui_language.clone();
        let palette = ui_palette(&self.settings.ui_theme);
        let testing = self.background_task_label == Some(crate::ui::tasks::TEST_AUDIO_DEVICE_LABEL);
        ui.label("");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.background_task.is_none(),
                    egui::Button::new(ui_text(&language, UiTextKey::MicRefresh)),
                )
                .on_hover_text(ui_text(&language, UiTextKey::MicRefreshHelp))
                .clicked()
            {
                self.run_list_audio_devices();
            }
            if ui
                .add_enabled(
                    self.background_task.is_none(),
                    egui::Button::new(ui_text(&language, UiTextKey::MicTest)),
                )
                .on_hover_text(ui_text(&language, UiTextKey::MicTestHelp))
                .clicked()
            {
                self.run_test_audio_device();
            }
            self.microphone_test_status(ui, &language, palette, testing);
        });
        ui.end_row();
    }

    /// Clear the inline microphone-test result when the selected device changed
    /// since `device_before`. The ✓/✗ outcome was produced for the previous
    /// device, so leaving it pinned next to a newly-picked device would show a
    /// stale, misleading verdict. A new test must be run for the new device.
    pub(in crate::ui) fn clear_device_test_result_if_device_changed(
        &mut self,
        device_before: &str,
    ) {
        if self.settings.audio_device != device_before {
            self.device_test_result = None;
        }
    }

    /// Render the inline microphone-test status next to the Test button: a
    /// "Testing…" spinner while the worker runs, then the ✓/⚠/✗ outcome (or an
    /// error message) once it finishes. Reads `device_test_result` (transient UI
    /// state, never persisted).
    fn microphone_test_status(
        &self,
        ui: &mut egui::Ui,
        language: &str,
        palette: UiPalette,
        testing: bool,
    ) {
        if testing {
            ui.add_space(4.0);
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(
                egui::RichText::new(ui_text(language, UiTextKey::MicTesting))
                    .color(palette.text_muted),
            );
            return;
        }
        let Some(result) = self.device_test_result.as_ref() else {
            return;
        };
        ui.add_space(4.0);
        match result {
            Ok(display) => {
                let (icon, color, text) = microphone_test_parts(display, language, palette);
                ui.label(icon_text(icon, text).color(color));
            }
            Err(message) => {
                ui.label(
                    icon_text(egui_material_icons::icons::ICON_WARNING.codepoint, message)
                        .color(palette.warn_text),
                );
            }
        }
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
            self.test_cloud_api_indicator(ui);
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

    /// Render the inline ✓/✗/testing indicator next to "Test cloud API" from the
    /// stored `stt_api_key_status` and whether the cloud-API check is in flight.
    /// Delegates to the shared `render_api_check_indicator` shell.
    fn test_cloud_api_indicator(&self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        let in_flight = self.background_task_label == Some("cloud API check");
        render_api_check_indicator(ui, &self.stt_api_key_status, in_flight, palette);
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

/// Map a parsed microphone-test display model to its inline (icon, colour,
/// localized text) for rendering next to the Test button:
///   ✓ green  "Works"
///   ✓ green  "Works via DirectSound (48 kHz, resampled)" (works via a fallback
///            path — the caveat is informational text, not a warning)
///   ✗ red    "Cannot be used: <reason>"
/// Pure aside from the localization lookup, so the works/cannot branching and
/// the caveat-detail assembly are unit-testable without an egui context.
pub(in crate::ui) fn microphone_test_parts(
    display: &DeviceTestDisplay,
    language: &str,
    palette: UiPalette,
) -> (&'static str, egui::Color32, String) {
    use egui_material_icons::icons;
    match display.outcome {
        DeviceTestOutcome::Works => (
            icons::ICON_CHECK_CIRCLE.codepoint,
            palette.ok_text,
            ui_text(language, UiTextKey::MicTestWorks).to_owned(),
        ),
        // A device that opens via a fallback (DirectSound/MME) or at a resampled
        // rate still WORKS — render it as a green check, not an amber warning, so
        // the user isn't misled into thinking there's a problem. The "via
        // DirectSound / resampled" detail is informational, carried in the text.
        DeviceTestOutcome::WorksWithCaveat => (
            icons::ICON_CHECK_CIRCLE.codepoint,
            palette.ok_text,
            microphone_test_caveat_text(display, language),
        ),
        DeviceTestOutcome::Cannot => {
            let reason = display
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|reason| !reason.is_empty());
            let text = match reason {
                Some(reason) => {
                    format!("{}: {reason}", ui_text(language, UiTextKey::MicTestCannot))
                }
                None => ui_text(language, UiTextKey::MicTestCannot).to_owned(),
            };
            (icons::ICON_WARNING.codepoint, palette.error_text, text)
        }
    }
}

/// Build the ⚠ caveat detail line, e.g. "Works via DirectSound (48 kHz,
/// resampled)". The endpoint and rate come straight from the worker result; the
/// "via <endpoint>" clause is dropped for the plain `default`/`wasapi` path so a
/// resample-only caveat reads "Works (48 kHz, resampled)".
fn microphone_test_caveat_text(display: &DeviceTestDisplay, language: &str) -> String {
    let mut detail: Vec<String> = Vec::new();
    if let Some(rate) = display.samplerate {
        detail.push(samplerate_khz_label(rate));
    }
    if display.resampled {
        detail.push(ui_text(language, UiTextKey::MicTestResampled).to_owned());
    }
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(" ({})", detail.join(", "))
    };
    let via_fallback = matches!(
        display.endpoint.as_deref(),
        Some("directsound") | Some("mme")
    );
    if via_fallback {
        let endpoint = endpoint_label(display.endpoint.as_deref().unwrap_or("default"));
        format!(
            "{} {endpoint}{suffix}",
            ui_text(language, UiTextKey::MicTestWorksVia)
        )
    } else {
        format!("{}{suffix}", ui_text(language, UiTextKey::MicTestWorks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::ui_palette;

    fn works(endpoint: &str, samplerate: Option<u32>, resampled: bool) -> DeviceTestDisplay {
        DeviceTestDisplay {
            outcome: if resampled || matches!(endpoint, "directsound" | "mme") {
                DeviceTestOutcome::WorksWithCaveat
            } else {
                DeviceTestOutcome::Works
            },
            endpoint: Some(endpoint.to_owned()),
            samplerate,
            resampled,
            reason: None,
        }
    }

    #[test]
    fn clean_wasapi_renders_green_works() {
        let palette = ui_palette("dark");
        let display = works("wasapi", Some(16000), false);
        let (icon, color, text) = microphone_test_parts(&display, "en", palette);
        assert_eq!(
            icon,
            egui_material_icons::icons::ICON_CHECK_CIRCLE.codepoint
        );
        assert_eq!(color, palette.ok_text);
        assert_eq!(text, "Works");
    }

    #[test]
    fn directsound_renders_green_works_via() {
        let palette = ui_palette("dark");
        let display = works("directsound", Some(48000), false);
        let (icon, color, text) = microphone_test_parts(&display, "en", palette);
        // Works-via-fallback is still success: green check, not an amber warning.
        assert_eq!(
            icon,
            egui_material_icons::icons::ICON_CHECK_CIRCLE.codepoint
        );
        assert_eq!(color, palette.ok_text);
        assert_eq!(text, "Works via DirectSound (48 kHz)");
    }

    #[test]
    fn resampled_wasapi_renders_green_with_resampled_note() {
        let palette = ui_palette("dark");
        let display = works("wasapi", Some(48000), true);
        let (icon, color, text) = microphone_test_parts(&display, "en", palette);
        assert_eq!(
            icon,
            egui_material_icons::icons::ICON_CHECK_CIRCLE.codepoint
        );
        assert_eq!(color, palette.ok_text);
        // Plain WASAPI path → no "via", just the rate + resampled note.
        assert_eq!(text, "Works (48 kHz, resampled)");
    }

    #[test]
    fn cannot_renders_red_with_reason() {
        let palette = ui_palette("dark");
        let display = DeviceTestDisplay {
            outcome: DeviceTestOutcome::Cannot,
            endpoint: None,
            samplerate: None,
            resampled: false,
            reason: Some("device not found".to_owned()),
        };
        let (icon, color, text) = microphone_test_parts(&display, "en", palette);
        assert_eq!(icon, egui_material_icons::icons::ICON_WARNING.codepoint);
        assert_eq!(color, palette.error_text);
        assert_eq!(text, "Cannot be used: device not found");
    }

    #[test]
    fn danish_localizes_the_outcome_words() {
        let palette = ui_palette("dark");
        assert_eq!(
            microphone_test_parts(&works("wasapi", Some(16000), false), "da", palette).2,
            "Virker"
        );
        assert_eq!(
            microphone_test_parts(&works("directsound", Some(48000), false), "da", palette).2,
            "Virker via DirectSound (48 kHz)"
        );
        let cannot = DeviceTestDisplay {
            outcome: DeviceTestOutcome::Cannot,
            endpoint: None,
            samplerate: None,
            resampled: false,
            reason: Some("device not found".to_owned()),
        };
        assert_eq!(
            microphone_test_parts(&cannot, "da", palette).2,
            "Kan ikke bruges: device not found"
        );
    }
}
