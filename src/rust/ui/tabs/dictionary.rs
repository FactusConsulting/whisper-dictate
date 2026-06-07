use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn dictionary_tab(&mut self, ui: &mut egui::Ui) {
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
        settings_grid("dictionary_settings")
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
}
