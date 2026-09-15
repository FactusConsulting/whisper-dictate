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
}
