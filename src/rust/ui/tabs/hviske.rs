use super::*;

#[derive(Clone, Copy)]
struct HviskeModel {
    id: &'static str,
    label: &'static str,
}

const MODELS: [HviskeModel; 2] = [
    HviskeModel {
        id: crate::cloud_api::HVISKE_V53,
        label: "Hviske v5.3",
    },
    HviskeModel {
        id: crate::cloud_api::HVISKE_V6,
        label: "Hviske v6",
    },
];
const LICENSE_URL: &str = "https://creativecommons.org/licenses/by-nc/4.0/";
const COMMERCIAL_URL: &str = "https://syv.ai/kontakt";

impl WhisperDictateApp {
    pub(super) fn hviske_model_options(&mut self, ui: &mut egui::Ui, enabled: bool) {
        let dialog_id = ui.scope_id().with("hviske-license");
        ui.label("");
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal_wrapped(|ui| {
                for model in MODELS {
                    if ui.button(model.label).clicked() {
                        ui.ctx().data_mut(|data| data.insert_temp(dialog_id, model));
                    }
                }
            });
        });
        ui.end_row();
        if !enabled {
            ui.ctx()
                .data_mut(|data| data.remove::<HviskeModel>(dialog_id));
            return;
        }
        if crate::cloud_api::is_hviske_model(&self.settings.stt_model) {
            ui.label("");
            ui.hyperlink_to("Hviske weights: CC BY-NC 4.0 (noncommercial)", LICENSE_URL);
            ui.end_row();
        }
        let Some(candidate) = ui
            .ctx()
            .data(|data| data.get_temp::<HviskeModel>(dialog_id))
        else {
            return;
        };
        let dialog = egui::Modal::new(dialog_id).show(ui.ctx(), |ui| {
            ui.set_max_width(440.0);
            ui.heading(format!("Use {}?", candidate.label));
            ui.label("syv.ai releases these weights for noncommercial use under CC BY-NC 4.0. For commercial use, obtain a separate license from syv.ai.");
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to("License", LICENSE_URL);
                ui.hyperlink_to("Contact syv.ai", COMMERCIAL_URL);
            });
            ui.label("Whisper Dictate does not download or run this model. You need to configure an OpenAI-compatible server that serves it. Dictation sends audio to that server; only a server on this PC keeps it on this PC.");
            ui.horizontal(|ui| {
                let confirmed = ui.button("Use model").clicked();
                let cancelled = ui.button("Cancel").clicked();
                (confirmed, cancelled)
            }).inner
        });
        let (confirmed, cancelled) = dialog.inner;
        if confirmed {
            self.settings.stt_model = candidate.id.to_owned();
            self.settings_status = format!(
                "{} selected. Set the server URL, then save. This model transcribes Danish.",
                candidate.label
            );
        }
        if confirmed || cancelled || dialog.should_close() {
            ui.ctx()
                .data_mut(|data| data.remove::<HviskeModel>(dialog_id));
        }
    }
}
