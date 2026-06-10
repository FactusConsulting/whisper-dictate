use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn output_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        ui.heading("Output");
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            section_label(ui, "Log view", palette);
            self.log_mode_selector(ui, palette);
            ui.add_space(12.0);
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::UiTheme),
                palette,
            );
            let ui_language = self.settings.ui_language.clone();
            theme_toggle(ui, &mut self.settings.ui_theme, palette, &ui_language);
            ui.add_space(12.0);
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::UiLanguage),
                palette,
            );
            language_toggle(ui, &mut self.settings.ui_language, palette);
        });
        ui.add_space(14.0);
        self.session_panel(ui, palette);
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
        settings_grid("output_settings")
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
                    "Feedback sounds",
                    &mut self.settings.feedback_sounds,
                    "Play a short audio cue when recording starts and stops. Useful for headless/autostart usage where the console is hidden (Terminal=false).",
                );
                checkbox_help(
                    ui,
                    "Feedback notifications",
                    &mut self.settings.feedback_notify,
                    "Show a desktop notification when an error occurs (model load failure, audio capture lost, injection failure). Useful for headless/autostart usage where the console is hidden.",
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
}
