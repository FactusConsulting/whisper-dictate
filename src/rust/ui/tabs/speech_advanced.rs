//! Speech tab rows that are advanced-only (hidden in Simple settings mode).
//! Split out of `speech.rs` — already the largest tab file, and told to stay
//! that way — so wiring the Simple/Advanced gate into it doesn't grow it
//! further: each helper here decides its own visibility via
//! [`setting_visible`], so `core_tab` just calls it unconditionally.

use super::*;

#[cfg(windows)]
fn windows_shortcut_row(
    ui: &mut egui::Ui,
    palette: UiPalette,
    label: &str,
    shortcut: &mut String,
    help: &str,
    validate: impl FnOnce(&str) -> Result<(), String>,
) {
    text_help_short(ui, label, shortcut, help);
    if shortcut.trim().is_empty() {
        return;
    }
    let (message, color) = match validate(shortcut) {
        Ok(()) => ("Shortcut syntax is valid".to_owned(), palette.ok_text),
        Err(error) => (error, palette.warn_text),
    };
    ui.label("");
    ui.label(egui::RichText::new(message).color(color));
    ui.end_row();
}

impl WhisperDictateApp {
    #[cfg(windows)]
    pub(super) fn speech_windows_shortcut_rows(
        &mut self,
        ui: &mut egui::Ui,
        mode: SettingsMode,
        palette: UiPalette,
    ) {
        let settings = &mut self.settings;
        if setting_visible(mode, "copy_last_hotkey") {
            windows_shortcut_row(
                ui,
                palette,
                "Copy last shortcut",
                &mut settings.copy_last_hotkey,
                "Optional Windows shortcut, such as ctrl+shift+f8. Copies the most recent local-history transcript without starting dictation. Leave blank to disable; restart the runtime after changing it.",
                |shortcut| validate_copy_last_hotkey(shortcut, &settings.key, &settings.paste_last_hotkey),
            );
        }
        if setting_visible(mode, "paste_last_hotkey") {
            windows_shortcut_row(
                ui,
                palette,
                "Paste last shortcut",
                &mut settings.paste_last_hotkey,
                "Optional Windows shortcut, such as ctrl+shift+f9. Re-injects the most recent local-history transcript into the focused window, including transcripts whose original injection failed. Leave blank to disable; restart the runtime after changing it.",
                |shortcut| validate_paste_last_hotkey(shortcut, &settings.key, &settings.copy_last_hotkey),
            );
        }
        if setting_visible(mode, "cycle_mode_hotkey") {
            windows_shortcut_row(
                ui,
                palette,
                "Cycle mode shortcut",
                &mut settings.cycle_mode_hotkey,
                "Optional Windows shortcut, such as ctrl+shift+f10. Rotates the post-processing mode through raw, clean, prompt, terminal, slack, email, and bullets, saves it, and reports the new mode on the runtime log. Leave blank to disable; restart the runtime after changing it.",
                |shortcut| validate_mode_hotkey(shortcut, &settings.key, "cycle mode"),
            );
        }
        if setting_visible(mode, "raw_mode_hotkey") {
            windows_shortcut_row(
                ui,
                palette,
                "Raw mode shortcut",
                &mut settings.raw_mode_hotkey,
                "Optional Windows shortcut, such as ctrl+shift+f11. Switches the post-processing mode to raw, saves it, and reports the change on the runtime log. Leave blank to disable; restart the runtime after changing it.",
                |shortcut| validate_mode_hotkey(shortcut, &settings.key, "raw mode"),
            );
        }
        if setting_visible(mode, "clean_mode_hotkey") {
            windows_shortcut_row(
                ui,
                palette,
                "Clean mode shortcut",
                &mut settings.clean_mode_hotkey,
                "Optional Windows shortcut, such as ctrl+shift+f6. Switches the post-processing mode to clean, saves it, and reports the change on the runtime log. Leave blank to disable; restart the runtime after changing it.",
                |shortcut| validate_mode_hotkey(shortcut, &settings.key, "clean mode"),
            );
        }
    }

    /// The "Device" row in the Speech → General group (`device`; advanced
    /// see the schema change in settings_schema.json for #settings-simple-mode).
    pub(in crate::ui) fn speech_device_row(
        &mut self,
        ui: &mut egui::Ui,
        mode: SettingsMode,
        available: bool,
        nemotron_in_process: bool,
    ) {
        if !available || !setting_visible(mode, "device") {
            return;
        }
        let devices = local_speech_devices(nemotron_in_process);
        combo_help_short(
            ui,
            "Device",
            &mut self.settings.device,
            devices,
            "GPU device for local speech recognition. Auto selects a compatible GPU. CPU inference is not supported.",
        );
    }

    pub(in crate::ui) fn show_nemotron_route_warning(
        &self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        provider: CloudProvider,
        in_process: bool,
    ) {
        if provider != CloudProvider::Nemotron {
            return;
        }
        let checking = "Checking for a GPU compatible with local Nemotron…";
        let warning = if in_process && !self.selected_nemotron_model_fits() {
            if self.gpu_probe.is_some() {
                checking
            } else {
                "The configured local Nemotron model needs a compatible GPU with enough memory and a valid model file. Select English for hosted Nemotron, or use a multilingual endpoint in Advanced Speech settings."
            }
        } else if in_process && self.cpu_speech_device_selected() {
            "The saved CPU device cannot run local Nemotron. Select Auto in Advanced Speech settings."
        } else if nemotron_hosted_multilingual_warning_active(
            provider,
            &self.settings.stt_model,
            &self.settings.stt_base_url,
        ) {
            "NVIDIA's default hosted function is English-only. Add ?function-id=<multilingual NVCF function id>, or enter a gRPC URL for a multilingual NIM."
        } else if !self.nemotron_model_fits() && !self.nemotron_external_multilingual_endpoint() {
            if self.gpu_probe.is_some() {
                checking
            } else {
                "In-process Nemotron requires a compatible GPU. For Multilingual / Auto, configure an external endpoint in Advanced Speech settings."
            }
        } else {
            return;
        };
        ui.label("");
        ui.colored_label(palette.warn_text, warning);
        ui.end_row();
    }

    /// The "Linux keyboard layout" row (`xkb_layout`; advanced, non-Windows
    /// only).
    pub(in crate::ui) fn speech_xkb_layout_row(&mut self, ui: &mut egui::Ui, mode: SettingsMode) {
        if cfg!(windows) || !setting_visible(mode, "xkb_layout") {
            return;
        }
        if let Some(selected) = combo_help_labeled_short_selection(
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
        ) {
            self.record_nullable_selection("xkb_layout", &selected);
        }
    }

    /// The "Toggle mode" row (`toggle_mode`; advanced).
    pub(in crate::ui) fn speech_toggle_mode_row(&mut self, ui: &mut egui::Ui, mode: SettingsMode) {
        if !setting_visible(mode, "toggle_mode") {
            return;
        }
        checkbox_help(
            ui,
            "Toggle mode",
            &mut self.settings.toggle_mode,
            "Toggle mode: press the hotkey to start recording, press again to stop and transcribe — instead of holding it.",
        );
    }

    /// A small note explaining why the cloud provider/model pickers above are
    /// greyed out: Local-only mode blocks cloud/BYOK backends at the runtime
    /// level (see `crate::privacy`), but the `local_only` toggle itself lives
    /// in System → Integration, which Simple mode hides — without this note a
    /// Simple-mode user has no way to discover why Start fails.
    ///
    /// Gated on [`WhisperDictateApp::desired_local_only`] (the same
    /// effective-state helper `local_only_enabled`/`local_only_change_pending`
    /// use), NOT the raw `self.settings.local_only` field: local-only can
    /// also come from an
    /// ambient `VOICEPI_LOCAL_ONLY` environment override with NO `local_only`
    /// key in config.json at all, in which case `self.settings.local_only`
    /// stays `false` even though the runtime and `desired_local_only` both
    /// already treat cloud STT as blocked — the note would be silently
    /// suppressed in exactly the case a Simple-mode user (who cannot see the
    /// Integration toggle either way) needs it most. The remedy text is also
    /// worded for the actual source of the block — see
    /// [`local_only_blocks_cloud_note_text`].
    pub(in crate::ui) fn local_only_blocks_cloud_note(
        &self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        mode: SettingsMode,
        backend: SttBackendMode,
    ) {
        let local_only = self.desired_local_only();
        if !local_only_blocks_cloud_note_visible(mode, backend, local_only) {
            return;
        }
        ui.label("");
        ui.label(
            egui::RichText::new(local_only_blocks_cloud_note_text(
                self.local_only_ambient_env_override(),
            ))
            .color(palette.warn_text),
        );
        ui.end_row();
    }
}

fn local_speech_devices(nemotron_in_process: bool) -> &'static [&'static str] {
    use crate::whisper::device_options::{DEVICE_AUTO, DEVICE_VULKAN};
    // Auto can choose either pinned Nemotron GPU runtime. The probe does not
    // promise both; existing explicit values remain readable.
    if nemotron_in_process {
        &[DEVICE_AUTO]
    } else {
        &[DEVICE_AUTO, DEVICE_VULKAN]
    }
}

/// Whether the "Cloud STT API URL" row should render for `mode`. `stt_base_url`
/// is schema-advanced (hidden by default in Simple mode), but two cases need
/// it visible even there: a Custom provider has no other way to set its
/// endpoint (the default is a localhost placeholder), and the hosted-Nemotron
/// multilingual warning rendered below this row explicitly instructs the user
/// to edit it — showing that instruction next to a hidden field would be
/// actionable advice with no way to act on it.
pub(in crate::ui) fn stt_base_url_visible(
    mode: SettingsMode,
    provider: CloudProvider,
    stt_model: &str,
    stt_base_url: &str,
) -> bool {
    setting_visible(mode, "stt_base_url")
        || provider == CloudProvider::Custom
        || nemotron_hosted_multilingual_warning_active(provider, stt_model, stt_base_url)
}

/// Whether the "Hosted NVIDIA's default function is English-only" warning is
/// active: a Nemotron multilingual model selected against the hosted endpoint
/// without an explicit `?function-id=`. Shared between the warning's own
/// render condition and [`stt_base_url_visible`] so the two can never drift
/// apart (the warning must never be shown next to a hidden field).
pub(in crate::ui) fn nemotron_hosted_multilingual_warning_active(
    provider: CloudProvider,
    stt_model: &str,
    stt_base_url: &str,
) -> bool {
    provider == CloudProvider::Nemotron
        && crate::dictate::backends::cloud_transcribe::is_nemotron_multilingual_model(stt_model)
        && crate::cloud_api::is_hosted_nemotron_endpoint(stt_base_url)
        && !crate::cloud_api::has_custom_function_id(stt_base_url)
}

/// Whether the local-only-blocks-cloud note should render: Simple mode, a
/// cloud backend selected, and `local_only` actually on. Pure so the decision
/// is unit-testable without an egui context.
pub(in crate::ui) fn local_only_blocks_cloud_note_visible(
    mode: SettingsMode,
    backend: SttBackendMode,
    local_only: bool,
) -> bool {
    mode == SettingsMode::Simple && backend == SttBackendMode::Cloud && local_only
}

/// The remedy text for [`WhisperDictateApp::local_only_blocks_cloud_note`],
/// worded for the actual SOURCE of the block: when an
/// ambient `VOICEPI_LOCAL_ONLY` environment override is active, pointing the
/// user at Advanced -> System -> Integration is actively wrong advice — that
/// toggle only edits the persisted `local_only` setting, it cannot clear an
/// environment variable, so following it would leave the user just as
/// blocked with no idea why. Pure so the two wordings are unit-testable
/// without an egui context; `ambient_env_override` takes priority whenever
/// it's true, since in that case the Settings toggle would not fix anything
/// regardless of the persisted setting's own value.
pub(in crate::ui) fn local_only_blocks_cloud_note_text(ambient_env_override: bool) -> &'static str {
    if ambient_env_override {
        "Local-only mode blocks cloud speech recognition (set by the VOICEPI_LOCAL_ONLY \
         environment variable). Unset or clear VOICEPI_LOCAL_ONLY in your environment, then \
         restart, to disable it."
    } else {
        "Local-only mode blocks cloud speech recognition. Switch to Advanced (top of the \
         sidebar), then System → Integration, to disable it."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_speech_device_picker_only_offers_gpu_routes() {
        assert_eq!(local_speech_devices(false), &["auto", "vulkan"]);
        assert_eq!(local_speech_devices(true), &["auto"]);
    }

    #[test]
    fn base_url_hidden_in_simple_for_openai_and_groq() {
        assert!(!stt_base_url_visible(
            SettingsMode::Simple,
            CloudProvider::OpenAi,
            "gpt-4o-mini-transcribe",
            "https://api.openai.com/v1",
        ));
        assert!(!stt_base_url_visible(
            SettingsMode::Simple,
            CloudProvider::Groq,
            "whisper-large-v3-turbo",
            "https://api.groq.com/openai/v1",
        ));
    }

    #[test]
    fn base_url_stays_visible_in_simple_for_custom_provider() {
        assert!(stt_base_url_visible(
            SettingsMode::Simple,
            CloudProvider::Custom,
            "Systran/faster-whisper-large-v3",
            "http://localhost:8000/v1",
        ));
    }

    #[test]
    fn base_url_stays_visible_in_simple_when_the_hosted_multilingual_warning_is_active() {
        assert!(stt_base_url_visible(
            SettingsMode::Simple,
            CloudProvider::Nemotron,
            "nvidia/nemotron-asr-streaming",
            "grpc://grpc.nvcf.nvidia.com:443",
        ));
    }

    #[test]
    fn base_url_hidden_in_simple_for_nemotron_english_hosted() {
        // The English profile's hosted endpoint needs no manual edit, so the
        // warning (and therefore the URL row) stays hidden in Simple mode.
        assert!(!stt_base_url_visible(
            SettingsMode::Simple,
            CloudProvider::Nemotron,
            "nvidia/nemotron-speech-streaming-en-0.6b",
            "grpc://grpc.nvcf.nvidia.com:443",
        ));
    }

    #[test]
    fn base_url_hidden_in_simple_when_a_custom_function_id_is_already_set() {
        assert!(!stt_base_url_visible(
            SettingsMode::Simple,
            CloudProvider::Nemotron,
            "nvidia/nemotron-asr-streaming",
            "grpc://grpc.nvcf.nvidia.com:443?function-id=abc",
        ));
    }

    #[test]
    fn base_url_always_visible_in_advanced() {
        assert!(stt_base_url_visible(
            SettingsMode::Advanced,
            CloudProvider::OpenAi,
            "gpt-4o-mini-transcribe",
            "https://api.openai.com/v1",
        ));
    }

    #[test]
    fn local_only_note_visible_only_when_simple_cloud_and_blocked() {
        assert!(local_only_blocks_cloud_note_visible(
            SettingsMode::Simple,
            SttBackendMode::Cloud,
            true,
        ));
        assert!(!local_only_blocks_cloud_note_visible(
            SettingsMode::Simple,
            SttBackendMode::Cloud,
            false,
        ));
        assert!(!local_only_blocks_cloud_note_visible(
            SettingsMode::Simple,
            SttBackendMode::Whisper,
            true,
        ));
        assert!(!local_only_blocks_cloud_note_visible(
            SettingsMode::Advanced,
            SttBackendMode::Cloud,
            true,
        ));
    }

    /// The remedy text must name the actual source of
    /// the block, since the Settings-toggle instruction is actively wrong
    /// (and unreachable in Simple mode either way) when the block comes from
    /// the environment instead.
    #[test]
    fn note_text_names_the_actual_source_of_the_block() {
        let from_env = local_only_blocks_cloud_note_text(true);
        assert!(
            from_env.contains("VOICEPI_LOCAL_ONLY"),
            "env-sourced block must name VOICEPI_LOCAL_ONLY, got: {from_env}"
        );
        assert!(
            !from_env.contains("System"),
            "env-sourced block must not point at the Integration setting, got: {from_env}"
        );

        let from_setting = local_only_blocks_cloud_note_text(false);
        assert!(
            from_setting.contains("System"),
            "setting-sourced block must still point at System -> Integration, got: {from_setting}"
        );
        assert!(
            !from_setting.contains("VOICEPI_LOCAL_ONLY"),
            "setting-sourced block must not mention the env var, got: {from_setting}"
        );
    }
}
