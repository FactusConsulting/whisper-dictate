//! First-run local Whisper model setup shown on the Runtime tab.

use super::super::*;
use crate::ui::app::WHISPER_MODEL_PATH_ENV;
use crate::ui::whisper_models_state::ModelAvailability;
use crate::whisper::model_manager;

pub(super) fn setup_banner_message(
    external_path_is_set: bool,
    catalog_entry_exists: bool,
    visible_entry_exists: bool,
    availability: Option<ModelAvailability>,
    model: &str,
) -> Option<String> {
    if external_path_is_set {
        return Some(format!(
            "{WHISPER_MODEL_PATH_ENV} does not point to an existing GGML model file. Fix or remove it before recording."
        ));
    }
    if availability == Some(ModelAvailability::Available) {
        return None;
    }
    if !catalog_entry_exists {
        return Some(format!(
            "{model} is not supported. Choose a listed model before recording."
        ));
    }
    if !visible_entry_exists {
        return Some(format!(
            "{model} is a retained legacy model. Install it with `wd models download {model}`, or choose a current model."
        ));
    }
    if availability == Some(ModelAvailability::Checking) {
        return Some(format!(
            "Verifying {model}. Recording stays disabled until verification completes."
        ));
    }
    Some(format!("Download {model} before starting local dictation."))
}

pub(super) fn setup_banner_for_entry<T>(
    external_path_is_set: bool,
    entry: Option<T>,
    visible_entry_exists: bool,
    model: &str,
    availability_for: impl FnOnce(T) -> ModelAvailability,
) -> Option<String> {
    let catalog_entry_exists = entry.is_some();
    let availability = entry.map(availability_for);
    setup_banner_message(
        external_path_is_set,
        catalog_entry_exists,
        visible_entry_exists,
        availability,
        model,
    )
}

impl WhisperDictateApp {
    /// Keep a clean installation actionable from the first screen: local
    /// dictation cannot start until the selected GGML model has been verified,
    /// so show the same download control used in Settings directly above the
    /// runtime log. Existing custom model paths and cloud backends need no
    /// setup banner.
    pub(in crate::ui) fn selected_whisper_model_setup_banner(
        &mut self,
        ui: &mut egui::Ui,
        palette: UiPalette,
    ) {
        if SttBackendMode::from_raw(&self.settings.stt_backend) != SttBackendMode::Whisper
            || self.has_external_whisper_model_path()
        {
            return;
        }

        let model = self.settings.model.trim();
        let entry = model_manager::find(model);
        let visible_entry = entry.filter(|selected| {
            model_manager::visible_catalog().any(|candidate| candidate.name == selected.name)
        });
        let external_path_is_set = std::env::var_os(WHISPER_MODEL_PATH_ENV).is_some();
        let Some(message) = setup_banner_for_entry(
            external_path_is_set,
            entry,
            visible_entry.is_some(),
            model,
            |selected| self.whisper_model_downloads.availability_fast(selected),
        ) else {
            return;
        };

        ui.add_space(8.0);
        egui::Frame::default()
            .fill(palette.surface_active_bg)
            .stroke(egui::Stroke::new(1.0, palette.warn_text))
            .corner_radius(egui::CornerRadius::same(PANEL_RADIUS))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(
                    egui::RichText::new("Local Whisper model required")
                        .strong()
                        .color(palette.warn_text),
                );
                ui.label(message);
                ui.add_space(4.0);
                if !external_path_is_set {
                    if let Some(selected) = visible_entry {
                        let any_running = self.whisper_model_downloads.any_in_progress();
                        let downloads_blocked = self.local_only_downloads_blocked();
                        self.render_whisper_model_row(ui, selected, any_running, downloads_blocked);
                        return;
                    }
                }
                if ui.button("Open Speech settings").clicked() {
                    self.selected_tab = Tab::Speech;
                }
            });
    }
}
