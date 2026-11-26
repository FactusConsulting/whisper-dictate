use super::{egui, text_enabled, CloudProvider, WhisperDictateApp};
use crate::cloud_api::hviske_local::LocalModel;

const MODELS: [LocalModel; 2] = [LocalModel::V53, LocalModel::V6];
const LICENSE_URL: &str = "https://creativecommons.org/licenses/by-nc/4.0/";
const COMMERCIAL_URL: &str = "https://syv.ai/kontakt";
const SETUP_URL: &str = "https://github.com/FactusConsulting/whisper-dictate/blob/main/docs/STT_BACKENDS.md#run-hviske-on-this-pc";

#[derive(Clone, Copy)]
struct Selection {
    model: LocalModel,
    local: bool,
}

impl Selection {
    fn label(self) -> &'static str {
        match (self.model, self.local) {
            (LocalModel::V53, false) => "Hviske v5.3",
            (LocalModel::V6, false) => "Hviske v6",
            (LocalModel::V53, true) => "Hviske v5.3 (local GPU)",
            (LocalModel::V6, true) => "Hviske v6 (local GPU)",
        }
    }

    fn id(self) -> &'static str {
        if self.local {
            self.model.alias()
        } else {
            self.model.model_id()
        }
    }
}

impl WhisperDictateApp {
    pub(in crate::ui) fn local_hviske_model(&self) -> Option<LocalModel> {
        if self.current_cloud_provider() != CloudProvider::Custom {
            return None;
        }
        LocalModel::from_alias(&self.settings.stt_model)
    }

    fn local_hviske_fits(&self, model: LocalModel) -> bool {
        self.gpu_probe.is_none()
            && self
                .hviske_gpu_total_mib
                .is_some_and(|total| total >= model.required_mib())
    }

    pub(super) fn custom_model_field(&mut self, ui: &mut egui::Ui, enabled: bool, before: &str) {
        if let Some(model) = self.local_hviske_model() {
            ui.label("Speech model");
            ui.label(Selection { model, local: true }.label());
            ui.end_row();
            ui.label("");
            if ui
                .add_enabled(enabled, egui::Button::new("Use a different custom model"))
                .clicked()
            {
                self.settings.stt_model.clear();
            }
            ui.end_row();
            return;
        }

        text_enabled(
            ui,
            enabled,
            "Cloud STT model",
            &mut self.settings.stt_model,
            "Model ID your OpenAI-compatible server expects, for example Systran/faster-whisper-large-v3.",
        );
        if self.local_hviske_model().is_some() {
            before.clone_into(&mut self.settings.stt_model);
            self.settings_status = "Choose a local GPU preset to use Hviske on this PC.".to_owned();
        }
    }

    pub(super) fn local_hviske_can_test(&self) -> bool {
        self.local_hviske_model()
            .is_some_and(|model| self.local_hviske_fits(model))
            && self.background_task.is_none()
    }

    pub(super) fn hviske_model_options(&mut self, ui: &mut egui::Ui, enabled: bool) {
        let dialog_id = ui.scope_id().with("hviske-license");
        self.hviske_choices(ui, dialog_id, enabled, false);
        if MODELS.iter().any(|&model| self.local_hviske_fits(model)) {
            self.hviske_choices(ui, dialog_id, enabled, true);
        }
        if !enabled {
            ui.ctx()
                .data_mut(|data| data.remove::<Selection>(dialog_id));
            return;
        }
        let local_model = self.local_hviske_model();
        if crate::cloud_api::is_hviske_model(&self.settings.stt_model) || local_model.is_some() {
            ui.label("");
            ui.hyperlink_to("Hviske weights: CC BY-NC 4.0 (noncommercial)", LICENSE_URL);
            ui.end_row();
        }
        if let Some(model) = local_model {
            ui.label("Local GPU service");
            if !self.local_hviske_fits(model) && self.gpu_probe.is_none() {
                ui.label(format!(
                    "Unavailable: requires an NVIDIA GPU with native BF16 and at least {} MiB VRAM.",
                    model.required_mib()
                ));
            } else {
                ui.hyperlink_to("Setup and start instructions", SETUP_URL);
            }
            ui.end_row();
        }
        let Some(candidate) = ui.ctx().data(|data| data.get_temp::<Selection>(dialog_id)) else {
            return;
        };
        if candidate.local && !self.local_hviske_fits(candidate.model) {
            ui.ctx()
                .data_mut(|data| data.remove::<Selection>(dialog_id));
            return;
        }
        let dialog = egui::Modal::new(dialog_id).show(ui.ctx(), |ui| {
            ui.set_max_width(440.0);
            ui.heading(format!("Use {}?", candidate.label()));
            ui.label("syv.ai releases these weights for noncommercial use under CC BY-NC 4.0. For commercial use, obtain a separate license from syv.ai.");
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to("License", LICENSE_URL);
                ui.hyperlink_to("Contact syv.ai", COMMERCIAL_URL);
            });
            if candidate.local {
                ui.label("Audio stays on this PC. Start the separate CUDA service before testing or restarting dictation. Its setup downloads and runs syv.ai model code pinned to a specific revision. There is no CPU fallback.");
                ui.hyperlink_to("Local GPU setup", SETUP_URL);
            } else {
                ui.label("Whisper Dictate does not download or run this model. You need to configure an OpenAI-compatible server that serves it. Dictation sends audio to that server; only a server on this PC keeps it on this PC.");
            }
            ui.horizontal(|ui| {
                let confirmed = ui.button("Use model").clicked();
                let cancelled = ui.button("Cancel").clicked();
                (confirmed, cancelled)
            }).inner
        });
        let (confirmed, cancelled) = dialog.inner;
        if confirmed {
            self.settings.stt_model = candidate.id().to_owned();
            let next_step = if candidate.local {
                "Start the local GPU service, test it, then save and restart."
            } else {
                "Set the server URL, then save."
            };
            self.settings_status = format!(
                "{} selected. {next_step} This model transcribes Danish.",
                candidate.label()
            );
        }
        if confirmed || cancelled || dialog.should_close() {
            ui.ctx()
                .data_mut(|data| data.remove::<Selection>(dialog_id));
        }
    }

    fn hviske_choices(&self, ui: &mut egui::Ui, dialog_id: egui::Id, enabled: bool, local: bool) {
        ui.label(if local {
            "Run on this PC"
        } else {
            "Hosted or external"
        });
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal_wrapped(|ui| {
                for model in MODELS {
                    if local && !self.local_hviske_fits(model) {
                        continue;
                    }
                    let candidate = Selection { model, local };
                    if ui.button(candidate.label()).clicked() {
                        ui.ctx()
                            .data_mut(|data| data.insert_temp(dialog_id, candidate));
                    }
                }
            });
        });
        ui.end_row();
    }
}

#[cfg(test)]
#[path = "hviske_tests.rs"]
mod tests;
