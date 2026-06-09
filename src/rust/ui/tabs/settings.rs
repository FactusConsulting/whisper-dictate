use super::super::*;
use super::*;
use egui_material_icons::icons;

const SETTINGS_FOOTER_HEIGHT: f32 = 264.0;
const SETTINGS_FOOTER_CHROME_HEIGHT: f32 = 18.0;
const SETTINGS_MESSAGES_TOP_GAP: f32 = 14.0;
const SETTINGS_MESSAGES_MAX_HEIGHT: f32 = 88.0;

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
                // Span the full available width so the footer children (actions
                // row + messages card) track window resizes.
                ui.set_min_width(ui.available_width());
                self.settings_actions(ui);
                ui.add_space(SETTINGS_MESSAGES_TOP_GAP);
                self.settings_messages(ui);
            },
        );
    }

    fn settings_actions(&mut self, ui: &mut egui::Ui) {
        let is_dirty = self.has_unsaved_settings();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            // Save now lives in the sidebar (next to Reload config + status), so
            // it isn't repeated on every page. Reload + Reset stay page-local.
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
        let outer_avail = ui.available_width();
        let avail_h = ui.available_height();
        // Span the full available width and fill the remaining footer height so the
        // card keeps a uniform EDGE_MARGIN gap on the left, right and bottom (the
        // CentralPanel inset) instead of stopping short on the right/bottom.
        let inner_width = (outer_avail - 2.0 * EDGE_MARGIN).max(0.0);
        // No artificial floor: the card's own content (header + scroll area) is
        // the natural minimum, and filling exactly the available height avoids
        // forcing the card past the footer bottom on short windows.
        let inner_height = (avail_h - 2.0 * EDGE_MARGIN).max(0.0);
        messages_card_frame(palette).show(ui, |ui| {
            ui.set_width(inner_width);
            ui.set_min_height(inner_height);
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

/// Inner padding for the settings "Messages" card. Uses the shared
/// [`EDGE_MARGIN`] on every side so the card's internal padding matches the
/// uniform gap the card keeps from the panel edges.
fn messages_card_margin() -> egui::Margin {
    egui::Margin::symmetric(EDGE_MARGIN, EDGE_MARGIN)
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
    fn messages_card_uses_uniform_edge_margin() {
        let margin = messages_card_margin();
        // Every side uses the shared EDGE_MARGIN so the card padding is uniform.
        assert_eq!(margin.left, EDGE_MARGIN);
        assert_eq!(margin.right, EDGE_MARGIN);
        assert_eq!(margin.top, EDGE_MARGIN);
        assert_eq!(margin.bottom, EDGE_MARGIN);
    }
}
