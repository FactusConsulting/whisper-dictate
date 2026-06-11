use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn output_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        ui.heading("Output");
        ui.add_space(8.0);
        self.session_panel(ui, palette);
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
        settings_grid("output_settings")
            .show(ui, |ui| {
                combo_help_short(
                    ui,
                    "Inject mode",
                    &mut self.settings.inject_mode,
                    &["auto", "type", "paste", "print"],
                    "How text is inserted into the focused app. auto chooses the safest available strategy.",
                );
                combo_help_short(
                    ui,
                    "Format commands",
                    &mut self.settings.format_commands,
                    &["off", "en", "da", "both"],
                    "Enable spoken formatting commands such as punctuation and new lines.",
                );
                text_help(
                    ui,
                    "Command hook",
                    &mut self.settings.command_hook,
                    "Optional command run after accepted utterances for advanced automation.",
                );
                numeric_help(
                    ui,
                    "command_hook_timeout_ms",
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
            });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Preview history").clicked() {
                self.preview_history();
            }
            if ui.button("Open history").clicked() {
                self.open_history();
            }
        });
        if !self.history_preview.is_empty() {
            ui.label("History preview");
            let response = ui.add(
                egui::TextEdit::multiline(&mut self.history_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
            // The settings body lives in a vertical ScrollArea, so a freshly
            // loaded preview renders below the fold and the click reads as "did
            // nothing". Scroll the preview into view on the frame it loads, then
            // clear the one-shot flag (mirrors `runtime_log_scroll_to_bottom`).
            if self.scroll_to_history_preview {
                response.scroll_to_me(Some(egui::Align::Center));
                self.scroll_to_history_preview = false;
            }
        }
    }
}
