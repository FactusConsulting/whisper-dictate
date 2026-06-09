use super::super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn profiles_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");
        let palette = ui_palette(&self.settings.ui_theme);
        ui.label(
            egui::RichText::new(
                "Profiles override settings per target app. When the focused window's \
                 title or process matches a profile, that profile's settings apply just \
                 for that window — e.g. English + paste mode in your code editor, or a \
                 different language in Slack — and revert when you switch away.",
            )
            .color(palette.text_muted),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Format: a JSON array of objects, each with \"name\", a \"match\" on \
                 \"title\"/\"process\" (a string or list; case-insensitive substring), and \
                 \"settings\" (config keys to override). The first matching profile wins; \
                 the active one is logged as \"[profile] active: …\". The starter entry \
                 below is inert until you edit its match. Full reference: \
                 docs/CONFIGURATION.md → Target profiles.",
            )
            .color(palette.text_muted)
            .text_style(egui::TextStyle::Small),
        );
        ui.add_space(8.0);
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
