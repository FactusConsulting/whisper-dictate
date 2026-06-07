use super::super::*;
use super::*;
use egui_material_icons::icons;

impl WhisperDictateApp {
    pub(in crate::ui) fn sidebar(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        ui.set_min_height(ui.available_height());
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(icons::ICON_KEYBOARD_VOICE)
                    .size(25.0)
                    .color(palette.accent_blue),
            );
            ui.label(
                egui::RichText::new("whisper-dictate")
                    .size(20.0)
                    .strong()
                    .color(palette.accent_blue),
            );
        });
        ui.label(
            icon_text(
                icons::ICON_TUNE,
                ui_text(&self.settings.ui_language, UiTextKey::SidebarSubtitle),
            )
            .text_style(egui::TextStyle::Small)
            .color(palette.text_muted),
        );
        ui.add_space(18.0);

        for tab in Tab::ALL {
            let selected = self.selected_tab == tab;
            if nav_button(
                ui,
                selected,
                tab.icon(),
                tab.label(&self.settings.ui_language),
                palette,
            )
            .clicked()
            {
                self.selected_tab = tab;
            }
            ui.add_space(5.0);
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.label(
                egui::RichText::new(format!("v{}", self.app_version))
                    .text_style(egui::TextStyle::Small)
                    .color(palette.text_muted),
            );
            ui.add_space(8.0);
            if ui
                .add_enabled_ui(self.background_task.is_none(), |ui| {
                    ui.add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::new(icon_text(
                            icons::ICON_BUILD,
                            ui_text(&self.settings.ui_language, UiTextKey::InstallRepair),
                        )),
                    )
                })
                .inner
                .on_hover_text("Install or repair the local runtime environment.")
                .clicked()
            {
                self.run_install();
            }
            ui.add_space(6.0);
            if ui
                .add_sized(
                    [ui.available_width(), 34.0],
                    egui::Button::new(icon_text(
                        icons::ICON_HEALTH_AND_SAFETY,
                        ui_text(&self.settings.ui_language, UiTextKey::Doctor),
                    )),
                )
                .clicked()
            {
                self.run_doctor();
            }
            if ui
                .add_sized(
                    [ui.available_width(), 34.0],
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
            ui.add_space(10.0);
            sidebar_save_state(
                ui,
                self.has_unsaved_settings(),
                palette,
                &self.settings.ui_language,
            );
        });
    }

    pub(in crate::ui) fn top_status_bar(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let controls_width = top_status_controls_width();
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(
                    (ui.available_width() - controls_width).max(300.0),
                    ui.available_height(),
                ),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    status_card(
                        ui,
                        ui_text(&self.settings.ui_language, UiTextKey::Status),
                        icons::ICON_RADIO_BUTTON_CHECKED,
                        runtime_state_label(self.runtime_state, &self.settings.ui_language),
                        runtime_state_color(self.runtime_state, palette),
                        palette,
                    );
                    status_card(
                        ui,
                        ui_text(&self.settings.ui_language, UiTextKey::Backend),
                        icons::ICON_MODEL_TRAINING,
                        self.backend_summary(),
                        palette.accent_blue,
                        palette,
                    );
                    let (detail_label, detail_icon, detail_value) = self.stt_detail_summary();
                    status_card_wide(
                        ui,
                        detail_label,
                        detail_icon,
                        detail_value,
                        palette.accent_blue,
                        palette,
                    );
                    if let Some(label) = self.background_task_label {
                        status_card(
                            ui,
                            ui_text(&self.settings.ui_language, UiTextKey::Task),
                            icons::ICON_PENDING_ACTIONS,
                            label,
                            palette.warn_text,
                            palette,
                        );
                    }
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.global_controls(ui, palette);
            });
        });
    }

    pub(in crate::ui) fn global_controls(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let is_stopped = self.runtime_state == RuntimeState::Stopped;
        let is_active = !is_stopped;

        if ui
            .add_enabled(
                is_active,
                egui::Button::new(
                    icon_text(
                        icons::ICON_STOP,
                        ui_text(&self.settings.ui_language, UiTextKey::Stop),
                    )
                    .strong(),
                )
                .fill(palette.error_text)
                .min_size(egui::vec2(78.0, 34.0)),
            )
            .clicked()
        {
            self.stop_runtime();
        }
        if ui
            .add_enabled(
                is_stopped,
                egui::Button::new(
                    icon_text(
                        icons::ICON_PLAY_ARROW,
                        ui_text(&self.settings.ui_language, UiTextKey::Start),
                    )
                    .strong(),
                )
                .fill(palette.accent_dark)
                .min_size(egui::vec2(88.0, 34.0)),
            )
            .clicked()
        {
            self.start_runtime();
        }
    }
}

fn sidebar_save_state(ui: &mut egui::Ui, is_dirty: bool, palette: UiPalette, raw_language: &str) {
    inset_panel_frame(palette).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        if is_dirty {
            ui.label(
                icon_text(
                    icons::ICON_ERROR,
                    ui_text(raw_language, UiTextKey::UnsavedChanges),
                )
                .color(palette.warn_text),
            );
        } else {
            ui.label(
                icon_text(
                    icons::ICON_CHECK_CIRCLE,
                    ui_text(raw_language, UiTextKey::SettingsSaved),
                )
                .color(palette.ok_text),
            );
        }
    });
}

fn status_card(
    ui: &mut egui::Ui,
    label: &str,
    icon: &str,
    value: impl AsRef<str>,
    accent: egui::Color32,
    palette: UiPalette,
) {
    status_card_sized(ui, label, icon, value, accent, palette, 134.0);
}

fn status_card_wide(
    ui: &mut egui::Ui,
    label: &str,
    icon: &str,
    value: impl AsRef<str>,
    accent: egui::Color32,
    palette: UiPalette,
) {
    status_card_sized(ui, label, icon, value, accent, palette, 218.0);
}

fn status_card_sized(
    ui: &mut egui::Ui,
    label: &str,
    icon: &str,
    value: impl AsRef<str>,
    accent: egui::Color32,
    palette: UiPalette,
    min_width: f32,
) {
    let value = value.as_ref();
    egui::Frame::default()
        .fill(palette.surface_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(14.0, 9.0))
        .show(ui, |ui| {
            ui.set_min_width(min_width);
            ui.label(icon_text(icon, label).size(12.0).color(palette.text_muted));
            ui.label(egui::RichText::new(value).strong().color(accent))
                .on_hover_text(value);
        });
}

fn top_status_controls_width() -> f32 {
    186.0
}

fn runtime_state_color(state: RuntimeState, palette: UiPalette) -> egui::Color32 {
    match state {
        RuntimeState::Stopped => palette.text_muted,
        RuntimeState::Starting => palette.warn_text,
        RuntimeState::Running => palette.ok_text,
    }
}
