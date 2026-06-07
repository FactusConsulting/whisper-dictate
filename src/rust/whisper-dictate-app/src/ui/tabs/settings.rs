use super::super::*;
use super::*;
use egui_material_icons::icons;

const SETTINGS_FOOTER_HEIGHT: f32 = 264.0;
const SETTINGS_FOOTER_CHROME_HEIGHT: f32 = 18.0;
const SETTINGS_MESSAGES_TOP_GAP: f32 = 14.0;
const SETTINGS_MESSAGES_BOTTOM_GAP: f32 = 20.0;
const SETTINGS_MESSAGES_MAX_HEIGHT: f32 = 88.0;
// Vertical inner padding of the messages card (matches the shared panel frame).
const SETTINGS_MESSAGES_CARD_VERTICAL_PAD: f32 = 14.0;

impl WhisperDictateApp {
    pub(in crate::ui) fn settings_panel(
        &mut self,
        ui: &mut egui::Ui,
        body: fn(&mut Self, &mut egui::Ui),
    ) {
        let footer_height = SETTINGS_FOOTER_HEIGHT;
        let body_height =
            (ui.available_height() - footer_height - SETTINGS_FOOTER_CHROME_HEIGHT).max(0.0);

        egui::ScrollArea::vertical()
            .id_salt(format!("settings_body_{:?}", self.selected_tab))
            .auto_shrink([false, false])
            .max_height(body_height)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                body(self, ui);
            });

        ui.separator();
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), footer_height),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                // Pin the footer column to the full available width so the
                // children below (actions row + messages card) expand with the
                // window instead of collapsing to their content width.
                ui.set_min_width(ui.available_width());
                self.settings_actions(ui);
                ui.add_space(SETTINGS_MESSAGES_TOP_GAP);
                self.settings_messages(ui);
                ui.add_space(SETTINGS_MESSAGES_BOTTOM_GAP);
            },
        );
    }

    fn settings_actions(&mut self, ui: &mut egui::Ui) {
        let is_dirty = self.has_unsaved_settings();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            let mut save_button = egui::Button::new(if is_dirty {
                icon_text(
                    icons::ICON_SAVE,
                    ui_text(&self.settings.ui_language, UiTextKey::SaveSettingsDirty),
                )
                .strong()
            } else {
                icon_text(
                    icons::ICON_SAVE,
                    ui_text(&self.settings.ui_language, UiTextKey::SaveSettings),
                )
            });
            if is_dirty {
                save_button = save_button.fill(ui.visuals().selection.bg_fill);
            }
            if ui
                .add_enabled(is_dirty, save_button)
                .on_hover_text("Save changed settings and any edited cloud API key.")
                .clicked()
            {
                self.save_settings();
            }
            if ui
                .button(icon_text(
                    icons::ICON_REFRESH,
                    ui_text(&self.settings.ui_language, UiTextKey::ReloadConfig),
                ))
                .on_hover_text("Reload the config file from disk.")
                .clicked()
            {
                self.reload_settings();
            }
            if ui
                .button(icon_text(
                    icons::ICON_REFRESH,
                    ui_text(&self.settings.ui_language, UiTextKey::ResetPage),
                ))
                .on_hover_text("Reset only the settings shown on this page to the built-in defaults. Save to keep the reset.")
                .clicked()
            {
                self.reset_current_tab_settings();
            }
            if is_dirty {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    ui_text(&self.settings.ui_language, UiTextKey::UnsavedChanges),
                );
            }
        });
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            // Claim the full width so the "Config:" row tracks the window on
            // resize, matching the messages card below it.
            ui.set_min_width(ui.available_width());
            let palette = ui_palette(&self.settings.ui_theme);
            let config_chars = ((ui.available_width() / 8.0).floor() as usize).clamp(38, 92);
            ui.label(egui::RichText::new("Config:").color(palette.text_muted));
            ui.add(
                egui::Label::new(
                    egui::RichText::new(compact_label(&self.config_path, config_chars))
                        .monospace()
                        .color(palette.text),
                )
                .wrap(),
            )
            .on_hover_text(&self.config_path);
        });
    }

    fn reset_current_tab_settings(&mut self) {
        let tab = self.selected_tab;
        reset_tab_settings(&mut self.settings, tab);
        match tab {
            Tab::Speech => self.reload_stt_api_key(),
            Tab::Post => self.reload_post_api_key(),
            _ => {}
        }
        self.settings_status = format!(
            "Reset {} settings to defaults. Save settings to keep the reset.",
            tab.label(&self.settings.ui_language)
        );
    }

    /// Render the settings footer (actions row + messages card) and report the
    /// painted width of the messages card. Used by the layout regression test
    /// to prove the card fills the available width on resize.
    #[cfg(test)]
    pub(in crate::ui) fn footer_messages_card_width(&mut self, ui: &mut egui::Ui) -> f32 {
        self.settings_actions(ui);
        ui.add_space(SETTINGS_MESSAGES_TOP_GAP);
        let card = ui.scope(|ui| self.settings_messages(ui));
        card.response.rect.width()
    }

    fn settings_messages(&self, ui: &mut egui::Ui) {
        let mut messages = Vec::new();
        if !self.settings_status.trim().is_empty() {
            messages.push(self.settings_status.as_str());
        }
        match self.selected_tab {
            Tab::Speech if !self.stt_api_key_status.trim().is_empty() => {
                messages.push(self.stt_api_key_status.as_str());
            }
            Tab::Post if !self.post_api_key_status.trim().is_empty() => {
                messages.push(self.post_api_key_status.as_str());
            }
            _ => {}
        }

        let palette = ui_palette(&self.settings.ui_theme);
        messages_card_frame(palette).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(112.0);
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::Messages),
                palette,
            );
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .id_salt(format!("settings_messages_{:?}", self.selected_tab))
                .auto_shrink([false, false])
                .max_height(SETTINGS_MESSAGES_MAX_HEIGHT)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if messages.is_empty() {
                        ui.label(
                            egui::RichText::new(ui_text(
                                &self.settings.ui_language,
                                UiTextKey::NoMessages,
                            ))
                            .color(ui.visuals().weak_text_color()),
                        );
                    }
                    for message in messages {
                        status_label(ui, message, palette);
                    }
                });
        });
    }
}

/// Inner padding for the settings "Messages" card. The left/right padding is
/// intentionally tied to [`SETTINGS_MESSAGES_BOTTOM_GAP`] so the card's side
/// inset matches the gap left below it, keeping the footer visually balanced.
fn messages_card_margin() -> egui::Margin {
    egui::Margin::symmetric(
        SETTINGS_MESSAGES_BOTTOM_GAP,
        SETTINGS_MESSAGES_CARD_VERTICAL_PAD,
    )
}

fn messages_card_frame(palette: UiPalette) -> egui::Frame {
    panel_frame(palette).inner_margin(messages_card_margin())
}

pub(in crate::ui) fn reset_tab_settings(settings: &mut AppSettings, tab: Tab) {
    let defaults = AppSettings::default();
    match tab {
        Tab::Log => {}
        Tab::Speech => {
            settings.stt_backend = defaults.stt_backend;
            settings.model = defaults.model;
            settings.parakeet_model = defaults.parakeet_model;
            settings.stt_provider = defaults.stt_provider;
            settings.stt_model = defaults.stt_model;
            settings.stt_base_url = defaults.stt_base_url;
            settings.stt_timeout_ms = defaults.stt_timeout_ms;
            settings.device = defaults.device;
            settings.compute_type = defaults.compute_type;
            settings.lang = defaults.lang;
            settings.xkb_layout = defaults.xkb_layout;
            settings.key = defaults.key;
            settings.quit_key = defaults.quit_key;
            settings.quit_count = defaults.quit_count;
            settings.quit_window_ms = defaults.quit_window_ms;
        }
        Tab::Quality => {
            settings.beam_size = defaults.beam_size;
            settings.temperature = defaults.temperature;
            settings.context_min_seconds = defaults.context_min_seconds;
            settings.parakeet_min_seconds = defaults.parakeet_min_seconds;
            settings.release_tail_ms = defaults.release_tail_ms;
            settings.vad_threshold = defaults.vad_threshold;
            settings.vad_min_silence_ms = defaults.vad_min_silence_ms;
            settings.vad_speech_pad_ms = defaults.vad_speech_pad_ms;
            settings.target_dbfs = defaults.target_dbfs;
            settings.min_input_dbfs = defaults.min_input_dbfs;
            settings.min_snr_db = defaults.min_snr_db;
            settings.audio_ducking = defaults.audio_ducking;
            settings.audio_ducking_level = defaults.audio_ducking_level;
            settings.initial_prompt = defaults.initial_prompt;
        }
        Tab::Dictionary => {
            settings.dictionary = defaults.dictionary;
            settings.dictionary_enabled = defaults.dictionary_enabled;
            settings.dictionary_max_terms = defaults.dictionary_max_terms;
            settings.dictionary_prompt_chars = defaults.dictionary_prompt_chars;
        }
        Tab::Output => {
            settings.ui_theme = defaults.ui_theme;
            settings.ui_language = defaults.ui_language;
            settings.ui_log_view = defaults.ui_log_view;
            settings.inject_mode = defaults.inject_mode;
            settings.format_commands = defaults.format_commands;
            settings.inject_json = defaults.inject_json;
            settings.metrics_jsonl = defaults.metrics_jsonl;
            settings.command_hook = defaults.command_hook;
            settings.command_hook_timeout_ms = defaults.command_hook_timeout_ms;
            settings.history_enabled = defaults.history_enabled;
            settings.history_jsonl = defaults.history_jsonl;
            settings.local_only = defaults.local_only;
            settings.debug = defaults.debug;
            settings.stt_debug = defaults.stt_debug;
            settings.ui_text_scale = defaults.ui_text_scale;
        }
        Tab::Post => {
            settings.post_processor = defaults.post_processor;
            settings.post_mode = defaults.post_mode;
            settings.post_model = defaults.post_model;
            settings.post_base_url = defaults.post_base_url;
            settings.post_timeout_ms = defaults.post_timeout_ms;
            settings.post_max_input_chars = defaults.post_max_input_chars;
            settings.post_max_output_chars = defaults.post_max_output_chars;
            settings.post_redact = defaults.post_redact;
            settings.post_redact_terms = defaults.post_redact_terms;
        }
        Tab::Profiles => {
            settings.profiles_json = defaults.profiles_json;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_card_side_padding_matches_bottom_gap() {
        let margin = messages_card_margin();
        // The card's left/right inset is tied to the gap left below it so the
        // footer reads as a balanced block.
        assert_eq!(margin.left, SETTINGS_MESSAGES_BOTTOM_GAP);
        assert_eq!(margin.right, SETTINGS_MESSAGES_BOTTOM_GAP);
        assert_eq!(margin.top, SETTINGS_MESSAGES_CARD_VERTICAL_PAD);
        assert_eq!(margin.bottom, SETTINGS_MESSAGES_CARD_VERTICAL_PAD);
    }
}
