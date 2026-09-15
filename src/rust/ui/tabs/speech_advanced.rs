//! Speech tab rows that are advanced-only (hidden in Simple settings mode).
//! Split out of `speech.rs` — already the largest tab file, and told to stay
//! that way — so wiring the Simple/Advanced gate into it doesn't grow it
//! further: each helper here decides its own visibility via
//! [`setting_visible`], so `core_tab` just calls it unconditionally.

use super::speech::local_device_selector_enabled;
use super::*;

impl WhisperDictateApp {
    /// The "Device" row in the Speech → General group (`device`; advanced —
    /// see the schema change in settings_schema.json for #settings-simple-mode).
    pub(in crate::ui) fn speech_device_row(
        &mut self,
        ui: &mut egui::Ui,
        mode: SettingsMode,
        backend: SttBackendMode,
        nemotron_in_process: bool,
    ) {
        if !setting_visible(mode, "device") {
            return;
        }
        // Filter the offered values by the compiled-in local GPU backends. On
        // a CPU-only binary "vulkan" would silently fall back to CPU, so hide
        // it entirely and append a footnote to the help text explaining why
        // the menu is shorter (see crate::whisper::device_options for the
        // full rationale).
        let device_values = if nemotron_in_process {
            crate::whisper::device_options::available_device_values_for_provider("nemotron")
        } else {
            crate::whisper::device_options::available_device_values()
        };
        let device_help = if nemotron_in_process {
            "Local Nemotron inference device. auto tries its pinned Vulkan runtime, then CPU; choose CUDA only where offered by this platform.".to_owned()
        } else {
            format!(
                "Local inference device. auto chooses a GPU when available, otherwise CPU. \
                 Used by the local Whisper backend.{}",
                crate::whisper::device_options::missing_device_footnote(),
            )
        };
        combo_enabled_short(
            ui,
            local_device_selector_enabled(backend, nemotron_in_process),
            "Device",
            &mut self.settings.device,
            &device_values,
            &device_help,
        );
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
    pub(in crate::ui) fn local_only_blocks_cloud_note(
        &self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        mode: SettingsMode,
        backend: SttBackendMode,
    ) {
        if !local_only_blocks_cloud_note_visible(mode, backend, self.settings.local_only) {
            return;
        }
        ui.label("");
        ui.label(
            egui::RichText::new(
                "Local-only mode blocks cloud speech recognition. Switch to Advanced (top of the sidebar), then System → Integration, to disable it.",
            )
            .color(palette.warn_text),
        );
        ui.end_row();
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
