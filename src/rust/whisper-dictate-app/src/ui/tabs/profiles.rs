use super::super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn profiles_tab(&mut self, ui: &mut egui::Ui) {
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
