//! The System tab: app-level maintenance and settings that are not part of the
//! speech → text → output pipeline. It collects the runtime maintenance actions
//! that used to live in the sidebar (Reload config / Doctor + the config-file
//! shortcut) and the appearance/display/feedback/integration
//! settings that used to crowd the Output tab.
//!
//! Keeping these here lets the sidebar stay a slim navigator and the Output tab
//! stay focused on how dictated speech is turned into injected text.
//!
//! Only the always-visible Appearance section (`ui_theme`/`ui_language`, the
//! two System settings Simple mode keeps) renders directly here. Every other
//! section is advanced-only and lives in `system_advanced.rs`, gated
//! row-by-row via `setting_visible` — see that module's docs.

use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn use_default_metrics_jsonl_path(&mut self) {
        let path = default_metrics_jsonl_path(&self.config_path);
        self.settings.metrics_jsonl = path.clone();
        self.record_nullable_selection("metrics_jsonl", &path);
    }

    pub(in crate::ui) fn system_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        let mode = SettingsMode::from_raw(&self.settings.ui_settings_mode);
        ui.heading(ui_text(&self.settings.ui_language, UiTextKey::System));
        ui.add_space(8.0);

        self.system_maintenance_section(ui, palette, mode);

        // --- Appearance: theme + UI language. The only System settings kept
        // in Simple mode — gated per-row like everything else even though
        // both are allow-listed (always visible today), so a future change
        // to the allow-list is honoured here without further edits.
        ui.horizontal_wrapped(|ui| {
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::SystemAppearance),
                palette,
            );
            if setting_visible(mode, "ui_theme") {
                section_label(
                    ui,
                    ui_text(&self.settings.ui_language, UiTextKey::UiTheme),
                    palette,
                );
                let ui_language = self.settings.ui_language.clone();
                theme_toggle(ui, &mut self.settings.ui_theme, palette, &ui_language);
                ui.add_space(12.0);
            }
            if setting_visible(mode, "ui_language") {
                section_label(
                    ui,
                    ui_text(&self.settings.ui_language, UiTextKey::UiLanguage),
                    palette,
                );
                // A dropdown (rather than the old two-button toggle) so more UI
                // languages can be added later without crowding the row. Writes the
                // same raw "en"/"da" config values.
                let language = self.settings.ui_language.clone();
                let options = [
                    ("en", ui_text(&language, UiTextKey::English)),
                    ("da", ui_text(&language, UiTextKey::Danish)),
                ];
                let selected = options
                    .iter()
                    .find(|(raw, _)| *raw == self.settings.ui_language)
                    .map(|(_, display)| *display)
                    .unwrap_or_else(|| ui_text(&language, UiTextKey::English));
                egui::ComboBox::from_id_salt("ui_language_select")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (raw, display) in options {
                            ui.selectable_value(
                                &mut self.settings.ui_language,
                                raw.to_owned(),
                                display,
                            );
                        }
                    });
            }
        });

        self.system_display_section(ui, palette, mode);
        self.system_feedback_section(ui, palette, mode);
        self.system_updates_section(ui, palette, mode);
        self.system_diagnostics_section(ui, palette, mode);
        self.system_integration_section(ui, palette, mode);
    }
}

/// Suggested metrics path: `metrics.jsonl` next to the config file (the
/// app-data folder the user already knows). A relative config path with an
/// empty parent suggests a bare `metrics.jsonl` in the working directory.
/// Pure so it is unit-testable.
pub(in crate::ui) fn default_metrics_jsonl_path(config_path: &str) -> String {
    match std::path::Path::new(config_path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            parent.join("metrics.jsonl").to_string_lossy().into_owned()
        }
        _ => "metrics.jsonl".to_owned(),
    }
}

/// A compact UI-text-scale row: short text input flanked by "−"/"+" stepper
/// buttons that nudge the value by 0.05 within the theme's clamp range.
/// Placed here (tabs/system.rs — its only consumer) so it falls under the
/// `src/rust/ui/tabs/**` Sonar coverage exclusion for render code. The pure
/// `step_text_scale` logic stays in `text_scale.rs` where it is unit-tested.
pub(in crate::ui) fn text_scale_stepper(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    help: &str,
) {
    const STEP: f32 = 0.05;
    let show_help = label_with_help(ui, label, help);
    ui.horizontal(|ui| {
        if ui.small_button("−").on_hover_text("Smaller text").clicked() {
            *value = step_text_scale(value, -STEP);
        }
        ui.add(egui::TextEdit::singleline(value).desired_width(60.0));
        if ui.small_button("+").on_hover_text("Larger text").clicked() {
            *value = step_text_scale(value, STEP);
        }
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}
