//! The System tab: app-level maintenance and settings that are not part of the
//! speech → text → output pipeline. It collects the runtime maintenance actions
//! that used to live in the sidebar (Reload config / Doctor / Install-Repair +
//! the config-file shortcut) and the appearance/display/feedback/integration
//! settings that used to crowd the Output tab.
//!
//! Keeping these here lets the sidebar stay a slim navigator and the Output tab
//! stay focused on how dictated speech is turned into injected text.

use super::super::*;
use super::*;
use egui_material_icons::icons;

impl WhisperDictateApp {
    pub(in crate::ui) fn system_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        ui.heading(ui_text(&self.settings.ui_language, UiTextKey::System));
        ui.add_space(8.0);

        // --- Maintenance: the runtime actions that used to live in the sidebar.
        section_label(
            ui,
            ui_text(&self.settings.ui_language, UiTextKey::SystemMaintenance),
            palette,
        );
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            // Reload config + Install/Repair share the "another background task is
            // running" guard, matching their old sidebar enabled-state logic.
            let idle = self.background_task.is_none();
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new(icon_text(
                        icons::ICON_REFRESH,
                        ui_text(&self.settings.ui_language, UiTextKey::ReloadConfig),
                    )),
                )
                .on_hover_text("Reload the config file from disk.")
                .clicked()
            {
                self.reload_settings();
            }
            if ui
                .button(icon_text(
                    icons::ICON_HEALTH_AND_SAFETY,
                    ui_text(&self.settings.ui_language, UiTextKey::Doctor),
                ))
                .on_hover_text("Run environment diagnostics and write the result to the log.")
                .clicked()
            {
                self.run_doctor();
            }
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new(icon_text(
                        icons::ICON_BUILD,
                        ui_text(&self.settings.ui_language, UiTextKey::InstallRepair),
                    )),
                )
                .on_hover_text("Install or repair the local runtime environment.")
                .clicked()
            {
                self.run_install();
            }
            if ui
                .button(icon_text(
                    icons::ICON_INFO,
                    ui_text(&self.settings.ui_language, UiTextKey::ConfigFile),
                ))
                .on_hover_text(&self.config_path)
                .clicked()
            {
                self.open_config_folder();
            }
        });

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);

        // --- Appearance + Display: chrome that used to sit at the top of Output.
        ui.horizontal_wrapped(|ui| {
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::SystemAppearance),
                palette,
            );
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
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::SystemDisplay),
                palette,
            );
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::DictationView),
                palette,
            );
            self.log_mode_selector(ui, palette);
        });
        ui.add_space(12.0);
        settings_grid("system_appearance_settings").show(ui, |ui| {
            text_help(
                ui,
                "UI text scale",
                &mut self.settings.ui_text_scale,
                "Scale all text in this settings UI. Use 1.0 for default, 1.15 for larger text, or 1.3 for high-DPI displays.",
            );
        });

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);

        // --- Feedback: the audio/notification cues moved out of Output.
        section_label(
            ui,
            ui_text(&self.settings.ui_language, UiTextKey::SystemFeedback),
            palette,
        );
        ui.add_space(6.0);
        settings_grid("system_feedback_settings").show(ui, |ui| {
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
        });

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);

        // --- Integration: machine-readable outputs moved out of Output.
        section_label(
            ui,
            ui_text(&self.settings.ui_language, UiTextKey::SystemIntegration),
            palette,
        );
        ui.add_space(6.0);
        settings_grid("system_integration_settings").show(ui, |ui| {
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
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Preview metrics").clicked() {
                self.preview_metrics();
            }
            if ui.button("Open metrics").clicked() {
                self.open_metrics();
            }
        });
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

    /// Open the folder that contains the config file (not the file itself), so
    /// the user lands in a place where they can inspect/back up the JSON. Reuses
    /// the console-window-guarded `open_existing_path` helper.
    fn open_config_folder(&mut self) {
        let folder = std::path::Path::new(&self.config_path)
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from(&self.config_path));
        match config::open_existing_path(&folder) {
            Ok(path) => self.settings_status = format!("Opened config folder: {}", path.display()),
            Err(err) => self.settings_status = format!("Open config folder failed: {err}"),
        }
    }
}
