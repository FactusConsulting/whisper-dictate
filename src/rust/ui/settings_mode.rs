//! Simple/Advanced settings-visibility mode: pure, unit-testable rules
//! deciding which sidebar tabs and which individual settings are shown in
//! Simple mode, plus the sidebar toggle that switches it.
//!
//! Advanced mode is unchanged: every tab and every setting renders exactly as
//! it did before this feature existed. Simple mode narrows both down to a
//! curated subset:
//! - Tabs: a fixed, hand-picked set ([`tab_visible`]) — Quality, Dictionary,
//!   Post, and Profiles are advanced tuning surfaces with no single
//!   "essential" setting to fall back to, so the whole tab is hidden.
//! - Individual settings: [`setting_visible`] defers to the `advanced` flag
//!   in `settings_schema.json` (the single source of truth) for any
//!   schema-backed key, plus a small explicit allow-list
//!   ([`SIMPLE_ALLOW_LIST`]) for the handful of UI-only settings/sections
//!   that have no schema entry.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum SettingsMode {
    Simple,
    Advanced,
}

impl SettingsMode {
    pub(in crate::ui) const ALL: [SettingsMode; 2] = [SettingsMode::Simple, SettingsMode::Advanced];

    pub(in crate::ui) fn from_raw(raw: &str) -> Self {
        match raw {
            "simple" => Self::Simple,
            _ => Self::Advanced,
        }
    }

    pub(in crate::ui) fn id(self) -> &'static str {
        match self {
            SettingsMode::Simple => "simple",
            SettingsMode::Advanced => "advanced",
        }
    }

    pub(in crate::ui) fn label(self, raw_language: &str) -> &'static str {
        match self {
            SettingsMode::Simple => ui_text(raw_language, UiTextKey::SettingsModeSimple),
            SettingsMode::Advanced => ui_text(raw_language, UiTextKey::SettingsModeAdvanced),
        }
    }
}

/// Non-schema settings/sections that belong in Simple mode even though they
/// have no `settings_schema.json` entry: `stt_provider` is the UI-only cloud
/// provider picker, `stt_api_key` stands for the rendered cloud API-key
/// section (a block, not a single config field, and the key itself lives in
/// the OS credential store rather than config.json), and `ui_language` /
/// `ui_theme` are UI prefs in the same non-schema category as `ui_log_view`
/// and `ui_settings_mode` itself.
const SIMPLE_ALLOW_LIST: &[&str] = &["stt_provider", "stt_api_key", "ui_language", "ui_theme"];

/// Sidebar tabs shown in Simple mode.
const SIMPLE_TABS: &[Tab] = &[Tab::Log, Tab::Speech, Tab::Output, Tab::System];

/// Whether `tab` is shown in the sidebar for `mode`.
pub(in crate::ui) fn tab_visible(mode: SettingsMode, tab: Tab) -> bool {
    match mode {
        SettingsMode::Advanced => true,
        SettingsMode::Simple => SIMPLE_TABS.contains(&tab),
    }
}

/// Whether the setting or section named `key` is shown for `mode`. `key` is
/// either a `settings_schema.json` key (visibility follows its `advanced`
/// flag) or one of the UI-only names in [`SIMPLE_ALLOW_LIST`]. An unknown key
/// (neither in the schema nor the allow-list) is treated as advanced-only, so
/// a typo hides a field rather than silently exposing it.
pub(in crate::ui) fn setting_visible(mode: SettingsMode, key: &str) -> bool {
    match mode {
        SettingsMode::Advanced => true,
        SettingsMode::Simple => {
            SIMPLE_ALLOW_LIST.contains(&key)
                || config::runtime_settings()
                    .iter()
                    .any(|setting| setting.key == key && !setting.advanced)
        }
    }
}

/// The tab to select after switching to `mode`: `current` when it stays
/// visible, otherwise Speech — the natural landing tab, visible in both
/// modes.
pub(in crate::ui) fn fallback_tab_for_mode(mode: SettingsMode, current: Tab) -> Tab {
    if tab_visible(mode, current) {
        current
    } else {
        Tab::Speech
    }
}

impl WhisperDictateApp {
    /// The Simple/Advanced segmented toggle shown above the sidebar tab list.
    /// Mirrors `log_mode_selector`'s two-state pill styling. Applies and
    /// persists the choice immediately (see `set_settings_mode`) — the same
    /// instant-apply-and-save pattern as the log-view toggle, so switching
    /// modes never leaves the settings form looking "unsaved".
    pub(in crate::ui) fn settings_mode_selector(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let current = SettingsMode::from_raw(&self.settings.ui_settings_mode);
        let language = self.settings.ui_language.clone();
        let help = ui_text(&language, UiTextKey::SettingsModeHelp);
        let width = (ui.available_width() / 2.0).max(60.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for mode in SettingsMode::ALL {
                let selected = current == mode;
                let fill = if selected {
                    palette.accent_dark
                } else {
                    palette.surface_bg
                };
                let text = if selected {
                    egui::RichText::new(mode.label(&language))
                        .strong()
                        .color(palette.text)
                } else {
                    egui::RichText::new(mode.label(&language)).color(palette.text_muted)
                };
                let clicked = ui
                    .add_sized(
                        egui::vec2(width, 28.0),
                        egui::Button::new(text)
                            .fill(fill)
                            .stroke(egui::Stroke::new(1.0, palette.border_soft)),
                    )
                    .on_hover_text(help)
                    .clicked();
                if clicked && !selected {
                    self.set_settings_mode(mode);
                }
            }
        });
        ui.add_space(10.0);
    }
}
