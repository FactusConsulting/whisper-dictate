use super::speech_advanced::stt_base_url_visible;
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
                    icons::ICON_REFRESH.codepoint,
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

    pub(in crate::ui) fn reset_current_tab_settings(&mut self) {
        let tab = self.selected_tab;
        let mode = SettingsMode::from_raw(&self.settings.ui_settings_mode);
        reset_tab_settings(&mut self.settings, tab, mode);
        let nullable_values = match tab {
            Tab::Speech => vec![
                ("stt_model", self.settings.stt_model.clone()),
                ("audio_device", self.settings.audio_device.clone()),
                ("lang", self.settings.lang.clone()),
                ("xkb_layout", self.settings.xkb_layout.clone()),
            ],
            Tab::Quality => vec![("initial_prompt", self.settings.initial_prompt.clone())],
            Tab::Dictionary => vec![("dictionary", self.settings.dictionary.clone())],
            Tab::Output => vec![
                ("command_hook", self.settings.command_hook.clone()),
                ("history_jsonl", self.settings.history_jsonl.clone()),
            ],
            Tab::System => vec![("metrics_jsonl", self.settings.metrics_jsonl.clone())],
            Tab::Post => vec![("post_redact_terms", self.settings.post_redact_terms.clone())],
            Tab::Log | Tab::Profiles => Vec::new(),
        };
        // Only track a nullable-clear intent for a key that was ACTUALLY reset
        // above (Simple mode may have skipped it because it's hidden there) —
        // otherwise an already-empty hidden field would get flagged as an
        // explicit clear from an action that never touched it.
        for (key, value) in nullable_values {
            if setting_visible(mode, key) {
                self.record_nullable_selection(key, &value);
            }
        }
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
}

/// Reset the settings shown on `tab` to their built-in defaults. In Simple
/// mode this resets ONLY the fields that tab's Simple view actually shows
/// (Codex P1: the footer's "Reset page" used to silently wipe every
/// Advanced-only field on the page too — a Simple-mode user who can see only
/// theme/language on System, or engine/model/key/mic on Speech, had no way to
/// know Reset had also touched `device`, `local_only`, every Update/Feedback/
/// Diagnostics/Integration setting, and so on). Quality/Dictionary/Post/
/// Profiles are unconditional: those whole tabs are hidden from the sidebar
/// in Simple mode, so `reset_tab_settings` is only ever called for them while
/// `mode == Advanced` (`setting_visible(Advanced, _)` is always `true`
/// anyway, so gating them would be a no-op).
///
/// EXCEPTION to "gate every field on its own visibility" — coupled hidden
/// settings (Codex P1, third instance of this defect class after the
/// provider and model reset-ORDERING fixes below): when a hidden field's
/// value only makes sense paired with a visible one that just got reset,
/// gating it on its OWN visibility alone can leave the two mismatched
/// (e.g. provider reset to OpenAI, endpoint left pointing at the old
/// provider — a credential-routing bug, not a cosmetic one, since a
/// subsequent API call sends the new provider's key to the old provider's
/// URL). The couplings encoded here, after an audit of every Speech/Post
/// field for the same pattern:
/// - `stt_base_url` ↔ `stt_provider` (below): the endpoint always resets
///   whenever the provider does, regardless of the endpoint row's own
///   visibility.
/// - `stt_model` and `stt_timeout_ms` need NO such coupling: `stt_model` is
///   already unconditionally Simple-visible (same as `stt_provider`, so it
///   always resets in lockstep already), and `stt_timeout_ms` is a single
///   generic value with no per-provider variant — a stale timeout cannot
///   misroute anything.
/// - The Post-tab equivalents (`post_base_url`/`post_model`/
///   `post_timeout_ms` vs. `post_processor`) need no coupling logic either:
///   the whole Post tab is entirely hidden in Simple mode, so its branch
///   below is unconditional already and every field in it always resets
///   together regardless of mode.
pub(in crate::ui) fn reset_tab_settings(settings: &mut AppSettings, tab: Tab, mode: SettingsMode) {
    let defaults = AppSettings::default();
    match tab {
        Tab::Log => {}
        Tab::Speech => {
            // Resolve the provider AND the model from the settings AS THEY
            // STAND BEFORE any reset below — `stt_base_url_visible`'s
            // Custom-provider and hosted-Nemotron-multilingual-warning
            // exceptions must see the user's actual current provider/model,
            // not whatever `stt_provider`/`stt_model` get reset to a few
            // lines down (Codex: the provider case was already fixed; the
            // model case is the exact same ordering bug — clearing
            // `stt_model` first made the warning-based exception evaluate
            // against an empty model instead of the real one).
            let provider = CloudProvider::from_raw(&settings.stt_provider)
                .unwrap_or_else(|| CloudProvider::from_settings(settings));
            let original_stt_model = settings.stt_model.clone();
            // Whether `stt_provider` itself is about to be reset below.
            // `stt_base_url` is functionally COUPLED to it (a provider's
            // endpoint), not merely another independent Advanced-only field
            // — see the coupling note on the `stt_base_url` reset a few
            // lines down for why that distinction matters here (Codex P1).
            let provider_reset = setting_visible(mode, "stt_provider");
            if setting_visible(mode, "stt_backend") {
                settings.stt_backend = defaults.stt_backend;
            }
            if setting_visible(mode, "model") {
                settings.model = defaults.model;
            }
            if provider_reset {
                settings.stt_provider = defaults.stt_provider;
            }
            if setting_visible(mode, "stt_model") {
                settings.stt_model = defaults.stt_model;
            }
            // Reset `stt_base_url` whenever ITS ROW IS VISIBLE (unchanged),
            // OR whenever `stt_provider` itself was just reset above — even
            // if the endpoint row stays hidden (Codex P1, third instance of
            // this ordering/coupling defect class after the provider and
            // model snapshot fixes above). `stt_base_url` is not an
            // independent hidden Advanced field the "only touch what's
            // visible" Reset rule is meant to protect; it is the CURRENT
            // provider's endpoint. Resetting the visible provider back to
            // its default (e.g. Groq -> OpenAI) while leaving a stale
            // provider's endpoint in place produces a silently mismatched
            // provider/endpoint pair — a credential-routing bug, not a
            // cosmetic one: a subsequent "Test cloud API" call (or a real
            // request) sends the NEW provider's key to the OLD provider's
            // URL. A coupled hidden setting must reset with its visible
            // driver, full stop; only settings with NO such coupling stay
            // gated on their own visibility.
            if provider_reset
                || stt_base_url_visible(mode, provider, &original_stt_model, &settings.stt_base_url)
            {
                settings.stt_base_url = defaults.stt_base_url;
            }
            if setting_visible(mode, "stt_timeout_ms") {
                settings.stt_timeout_ms = defaults.stt_timeout_ms;
            }
            if setting_visible(mode, "device") {
                settings.device = defaults.device;
            }
            if setting_visible(mode, "audio_device") {
                settings.audio_device = defaults.audio_device;
            }
            if setting_visible(mode, "lang") {
                settings.lang = defaults.lang;
            }
            if setting_visible(mode, "xkb_layout") {
                settings.xkb_layout = defaults.xkb_layout;
            }
            if setting_visible(mode, "key") {
                settings.key = defaults.key;
            }
            if setting_visible(mode, "toggle_mode") {
                settings.toggle_mode = defaults.toggle_mode;
            }
        }
        Tab::Quality => {
            settings.max_chars_per_second = defaults.max_chars_per_second;
            settings.min_record_seconds = defaults.min_record_seconds;
            settings.release_tail_ms = defaults.release_tail_ms;
            settings.preview_seconds = defaults.preview_seconds;
            settings.max_record_s = defaults.max_record_s;
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
            if setting_visible(mode, "inject_mode") {
                settings.inject_mode = defaults.inject_mode;
            }
            if setting_visible(mode, "format_commands") {
                settings.format_commands = defaults.format_commands;
            }
            if setting_visible(mode, "command_hook") {
                settings.command_hook = defaults.command_hook;
            }
            if setting_visible(mode, "command_hook_timeout_ms") {
                settings.command_hook_timeout_ms = defaults.command_hook_timeout_ms;
            }
            if setting_visible(mode, "history_enabled") {
                settings.history_enabled = defaults.history_enabled;
            }
            if setting_visible(mode, "history_jsonl") {
                settings.history_jsonl = defaults.history_jsonl;
            }
        }
        Tab::System => {
            if setting_visible(mode, "ui_theme") {
                settings.ui_theme = defaults.ui_theme;
            }
            if setting_visible(mode, "ui_language") {
                settings.ui_language = defaults.ui_language;
            }
            if setting_visible(mode, "ui_log_view") {
                settings.ui_log_view = defaults.ui_log_view;
            }
            if setting_visible(mode, "ui_text_scale") {
                settings.ui_text_scale = defaults.ui_text_scale;
            }
            // `ui_settings_mode` is deliberately NOT reset here (independent
            // of mode-gating above): it is the Simple/Advanced switch itself,
            // applied instantly by `set_settings_mode` (never through this
            // page-scoped Reset action). Resetting it here would silently
            // flip the user from Simple back to Advanced mid-edit, mark the
            // form dirty, and skip the tab-selection fallback that only
            // `set_settings_mode` runs.
            //
            // `ui_autostart_runtime` is skipped for the same reason: it is an
            // instant-apply toggle that persists itself through a single-key
            // raw write (`set_autostart_runtime`), so clearing it here would
            // desynchronise the form from the file until an explicit Save.
            if setting_visible(mode, "update_check") {
                settings.update_check = defaults.update_check;
            }
            if setting_visible(mode, "update_check_interval_minutes") {
                settings.update_check_interval_minutes = defaults.update_check_interval_minutes;
            }
            if setting_visible(mode, "update_include_prereleases") {
                settings.update_include_prereleases = defaults.update_include_prereleases;
            }
            if setting_visible(mode, "json_output") {
                settings.inject_json = defaults.inject_json;
            }
            if setting_visible(mode, "metrics_jsonl") {
                settings.metrics_jsonl = defaults.metrics_jsonl;
            }
            if setting_visible(mode, "local_only") {
                settings.local_only = defaults.local_only;
            }
            if setting_visible(mode, "feedback_sounds") {
                settings.feedback_sounds = defaults.feedback_sounds;
            }
            if setting_visible(mode, "log_level") {
                settings.log_level = defaults.log_level;
            }
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
