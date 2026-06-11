use super::super::*;
use super::*;
use egui_material_icons::icons;

// The footer now only holds the single per-page Reset action row; Reload config
// + the config path moved to the System tab and status messages moved to the
// global bottom message bar.
const SETTINGS_FOOTER_HEIGHT: f32 = 40.0;
const SETTINGS_FOOTER_CHROME_HEIGHT: f32 = 18.0;

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
        self.settings_actions(ui);
    }

    fn settings_actions(&mut self, ui: &mut egui::Ui) {
        let is_dirty = self.has_unsaved_settings();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            // Save lives in the sidebar; Reload config + the config path moved to
            // the System tab. Only the per-page Reset stays here, so each settings
            // page keeps just the action that is scoped to that page.
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
    }

    fn reset_current_tab_settings(&mut self) {
        let tab = self.selected_tab;
        reset_tab_settings(&mut self.settings, tab);
        match tab {
            Tab::Speech => self.reload_stt_api_key(),
            Tab::Post => self.reload_post_api_key(),
            // Re-apply the prefill so the Metrics JSONL field is never left blank
            // after a reset — the schema default is "", but the UI always shows
            // the suggested path next to config.json.
            Tab::System if self.settings.metrics_jsonl.trim().is_empty() => {
                self.settings.metrics_jsonl = default_metrics_jsonl_path(&self.config_path);
            }
            _ => {}
        }
        self.settings_status = format!(
            "Reset {} settings to defaults. Save settings to keep the reset.",
            tab.label(&self.settings.ui_language)
        );
    }
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
            settings.audio_device = defaults.audio_device;
            settings.lang = defaults.lang;
            settings.xkb_layout = defaults.xkb_layout;
            settings.key = defaults.key;
            settings.toggle_mode = defaults.toggle_mode;
            settings.quit_key = defaults.quit_key;
            settings.quit_count = defaults.quit_count;
            settings.quit_window_ms = defaults.quit_window_ms;
        }
        Tab::Quality => {
            settings.beam_size = defaults.beam_size;
            settings.temperature = defaults.temperature;
            settings.context_min_seconds = defaults.context_min_seconds;
            settings.hallucination_guard = defaults.hallucination_guard;
            settings.max_chars_per_second = defaults.max_chars_per_second;
            settings.min_record_seconds = defaults.min_record_seconds;
            settings.parakeet_min_seconds = defaults.parakeet_min_seconds;
            settings.release_tail_ms = defaults.release_tail_ms;
            settings.preview_seconds = defaults.preview_seconds;
            settings.max_record_s = defaults.max_record_s;
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
            settings.inject_mode = defaults.inject_mode;
            settings.format_commands = defaults.format_commands;
            settings.command_hook = defaults.command_hook;
            settings.command_hook_timeout_ms = defaults.command_hook_timeout_ms;
            settings.history_enabled = defaults.history_enabled;
            settings.history_jsonl = defaults.history_jsonl;
            settings.local_only = defaults.local_only;
            settings.debug = defaults.debug;
            settings.stt_debug = defaults.stt_debug;
        }
        Tab::System => {
            settings.ui_theme = defaults.ui_theme;
            settings.ui_language = defaults.ui_language;
            settings.ui_log_view = defaults.ui_log_view;
            settings.ui_text_scale = defaults.ui_text_scale;
            settings.update_check = defaults.update_check;
            settings.update_check_interval_minutes = defaults.update_check_interval_minutes;
            settings.inject_json = defaults.inject_json;
            settings.metrics_jsonl = defaults.metrics_jsonl;
            settings.feedback_sounds = defaults.feedback_sounds;
            settings.feedback_notify = defaults.feedback_notify;
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
