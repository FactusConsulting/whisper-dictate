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
/// either a `settings_schema.json` key (visibility follows its `ui_simple`
/// flag) or one of the UI-only names in [`SIMPLE_ALLOW_LIST`]. An unknown key
/// (neither in the schema nor the allow-list) is treated as hidden, so a
/// typo hides a field rather than silently exposing it.
///
/// Deliberately keyed on `ui_simple`, NOT `advanced`: `advanced` also drives
/// the native setup wizard's basic/full prompt order, and a scripted
/// non-interactive setup depends on that order staying stable regardless of
/// which rows the desktop Settings UI's Simple mode happens to show. The two
/// flags are edited independently in `settings_schema.json`.
pub(in crate::ui) fn setting_visible(mode: SettingsMode, key: &str) -> bool {
    match mode {
        SettingsMode::Advanced => true,
        SettingsMode::Simple => {
            SIMPLE_ALLOW_LIST.contains(&key)
                || config::runtime_settings()
                    .iter()
                    .any(|setting| setting.key == key && setting.ui_simple)
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

    /// Apply and persist the Simple/Advanced settings-visibility preference.
    /// Mirrors `set_log_view`'s instant-apply intent (the sidebar toggle
    /// applies immediately and never commits the user's other pending edits
    /// — those stay in `settings` until an explicit Save) but persists
    /// differently: it writes ONLY the `ui_settings_mode` key through
    /// [`config::set_value`] — the same single-key read/merge/write path `wd
    /// config set` uses — rather than resaving the whole cached
    /// `saved_settings` snapshot. That snapshot can be stale the moment a
    /// concurrent `wd config set` (or a hand-edited config.json) has changed
    /// some OTHER key on disk since this session last loaded; resaving it
    /// wholesale would silently revert that external edit (Codex P2). The
    /// in-memory `saved_settings.ui_settings_mode` is updated only AFTER a
    /// successful write, so a failed save leaves `has_unsaved_settings`
    /// correctly reporting the mode as still pending instead of looking
    /// clean (Codex P2).
    ///
    /// Falls back the selected tab to Speech when it would otherwise become
    /// hidden (via `select_tab`), and — when switching TO Simple — surfaces
    /// a status hint if any still-pending edits are on a field Simple hides,
    /// so they are not silently forgotten out of view.
    pub(in crate::ui) fn set_settings_mode(&mut self, mode: SettingsMode) {
        self.settings.ui_settings_mode = mode.id().to_owned();
        self.select_tab(self.selected_tab);

        let mut messages = Vec::new();
        if mode == SettingsMode::Simple {
            let hidden_pending = self.hidden_pending_edit_keys();
            if !hidden_pending.is_empty() {
                messages.push(format!(
                    "Unsaved changes in {} advanced setting(s) not shown in Simple mode: {}.",
                    hidden_pending.len(),
                    hidden_pending.join(", ")
                ));
            }
        }
        match config::set_value("ui_settings_mode", mode.id(), &config::config_path()) {
            Ok(_) => self.saved_settings.ui_settings_mode = mode.id().to_owned(),
            Err(err) => {
                self.append_runtime_log(format!("[ui] could not persist settings mode: {err}"));
                messages.push(format!("Could not save {} mode: {err}.", mode.id()));
            }
        }
        if !messages.is_empty() {
            self.settings_status = messages.join(" ");
        }
    }

    /// The single choke point for writing `selected_tab`: clamps to a tab
    /// visible under the CURRENT settings mode via `fallback_tab_for_mode`, so
    /// a hidden tab (Quality/Dictionary/Post/Profiles in Simple mode) is never
    /// intentionally selected. `app.rs` also clamps `selected_tab`
    /// unconditionally every frame as a backstop for paths that change the
    /// mode without going through here (e.g. `reload_settings` reads a
    /// "simple" config off disk directly, and `wd config set` takes effect
    /// only after such a reload).
    pub(in crate::ui) fn select_tab(&mut self, tab: Tab) {
        let mode = SettingsMode::from_raw(&self.settings.ui_settings_mode);
        self.selected_tab = fallback_tab_for_mode(mode, tab);
    }

    /// Keys where `settings` differs from the on-disk `saved_settings` (a
    /// genuine pending edit) AND that Simple mode currently hides. Used to
    /// warn the user their edit is still pending but out of view, rather than
    /// silently losing track of it. Empty outside Simple mode.
    ///
    /// Diffs the serialized JSON representation rather than hand-listing every
    /// `AppSettings` field, so a newly-added field is covered automatically.
    /// The JSON key is usually the same as the `settings_schema.json` key
    /// (checked via `setting_visible`) except for the handful of fields whose
    /// struct name differs from their schema key (e.g. `inject_json` /
    /// `json_output`); those are already `advanced: true`, so this errs
    /// toward correctly flagging them as hidden rather than missing them.
    ///
    /// `ui_settings_mode` itself is always excluded: `set_settings_mode`
    /// calls this BEFORE persisting the new mode into `saved_settings` (see
    /// its own doc comment for why), so at that point `settings` and
    /// `saved_settings` briefly disagree on `ui_settings_mode` alone — that
    /// is the mode switch itself, not a "hidden pending edit" to warn about.
    ///
    /// Also covers pending edits that live OUTSIDE the `AppSettings`
    /// snapshot entirely (Codex P2): the Post processor's API key is not a
    /// config.json field — it's staged in `post_api_key_input` and only
    /// reaches the OS credential store on an explicit save — so an edited
    /// (or explicitly cleared) key never shows up in the JSON diff below.
    /// It's exactly as invisible as everything else on the Post tab, which
    /// is entirely hidden in Simple mode.
    pub(in crate::ui) fn hidden_pending_edit_keys(&self) -> Vec<String> {
        let mode = SettingsMode::from_raw(&self.settings.ui_settings_mode);
        if mode != SettingsMode::Simple {
            return Vec::new();
        }
        let (Some(current), Some(saved)) = (
            serde_json::to_value(&self.settings)
                .ok()
                .and_then(|value| value.as_object().cloned()),
            serde_json::to_value(&self.saved_settings)
                .ok()
                .and_then(|value| value.as_object().cloned()),
        ) else {
            return Vec::new();
        };
        let mut keys: std::collections::BTreeSet<String> = current
            .iter()
            .filter(|(key, _)| key.as_str() != "ui_settings_mode")
            .filter(|(key, value)| saved.get(key.as_str()) != Some(*value))
            .map(|(key, _)| key.clone())
            .filter(|key| !setting_visible(mode, key))
            .collect();
        // Codex: an explicit "clear to null" on a nullable field (e.g.
        // resetting Quality's `initial_prompt` while it is already an empty
        // string in BOTH `settings` and `saved_settings`) records the intent
        // in `explicit_nullable_clears` WITHOUT changing the serialized
        // value — the JSON diff above sees no difference and misses it,
        // even though the next Save persists an explicit `null` for that
        // key (suppressing any ambient environment-variable fallback). Merge
        // those keys in too, same hidden-under-`mode` filter as above.
        keys.extend(
            self.explicit_nullable_clears
                .iter()
                .filter(|key| !setting_visible(mode, key))
                .cloned(),
        );
        if self.post_api_key_input != self.saved_post_api_key_input {
            keys.insert("post_api_key".to_owned());
        }
        keys.into_iter().collect()
    }
}
