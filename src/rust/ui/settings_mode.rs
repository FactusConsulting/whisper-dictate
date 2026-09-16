//! Simple/Advanced settings-visibility mode: pure, unit-testable rules
//! deciding which sidebar tabs and which individual settings are shown in
//! Simple mode, plus the sidebar toggle that switches it.
//!
//! Advanced mode is unchanged: every tab and every setting renders exactly as
//! it did before this feature existed. Simple mode narrows both down to a
//! curated subset:
//! - Tabs: a fixed, hand-picked set ([`tab_visible`]) — Quality, Dictionary,
//!   and Profiles are advanced tuning surfaces with no single "essential"
//!   setting to fall back to, so the whole tab is hidden.
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
/// provider picker, `stt_api_key` / `post_api_key` stand for the rendered
/// cloud API-key sections (blocks, not single config fields, and the keys
/// themselves live in the OS credential store rather than config.json — a
/// cloud post-processor is unusable without one, so it follows the STT key
/// section into Simple), and `ui_language` /
/// `ui_theme` are UI prefs in the same non-schema category as `ui_log_view`
/// and `ui_settings_mode` itself. `ui_autostart_runtime` joins them as a
/// set-once-and-forget launch preference (#894) — exactly what Simple mode
/// is for.
const SIMPLE_ALLOW_LIST: &[&str] = &[
    "stt_provider",
    "stt_api_key",
    "post_api_key",
    "ui_language",
    "ui_theme",
    "ui_autostart_runtime",
];

/// Sidebar tabs shown in Simple mode. Post is included even though it is an
/// "advanced tuning surface": choosing a post-processor (and its rewrite
/// style) is a normal thing to want without leaving Simple, and the tab has
/// exactly two essential rows to fall back to. Every other row on it stays
/// Advanced-only via its `ui_simple` flag.
const SIMPLE_TABS: &[Tab] = &[Tab::Log, Tab::Speech, Tab::Output, Tab::Post, Tab::System];

/// Height of one Simple/Advanced selector button.
const MODE_BUTTON_HEIGHT: f32 = 28.0;

/// The 1 px border each selector button paints. `Button::stroke` paints
/// INSIDE the widget's allocated rect (it does not add to it), so a
/// horizontal pair with `item_spacing.x = 0` occupies exactly
/// `2 * button_width` — the stroke is accounted for by reserving it OUT OF
/// each button's own width in [`mode_selector_layout`] below (so the text
/// area inside the border has room), not by adding extra width on top.
pub(in crate::ui) const MODE_BUTTON_STROKE: f32 = 1.0;

/// Geometry for the Simple/Advanced selector at a given panel width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::ui) struct ModeSelectorLayout {
    /// Width to pass to `add_sized` for each of the two buttons.
    pub(in crate::ui) button_width: f32,
    /// True when the panel is too narrow for two readable labels side by
    /// side, so the buttons render as two stacked full-width rows instead.
    pub(in crate::ui) stacked: bool,
}

/// Decide the selector's geometry from what the sidebar panel ACTUALLY has
/// (`available_width`), not from a panel-blind floor.
///
/// `min_readable_button_width` is the width one button needs before a
/// side-by-side pair still reads (see `min_readable_mode_button_width`, which
/// measures the real font). Below that the pair is stacked as two full-width
/// rows — every label then gets the whole panel width, which is strictly
/// better than two unreadable slivers.
///
/// Pure (plain f32 in, plain struct out) so the whole rule is unit-testable
/// without an egui context; the render side only measures text and applies
/// the result. Fixes #895: the previous `(available_width / 2.0).max(60.0)`
/// ignored the panel entirely, so below ~120 px of available width the pair
/// was WIDER than the sidebar and the second button was clipped away.
pub(in crate::ui) fn mode_selector_layout(
    available_width: f32,
    min_readable_button_width: f32,
) -> ModeSelectorLayout {
    let usable = (available_width - 2.0 * MODE_BUTTON_STROKE).max(0.0);
    let side_by_side = usable / 2.0;
    if side_by_side >= min_readable_button_width {
        ModeSelectorLayout {
            button_width: side_by_side,
            stacked: false,
        }
    } else {
        ModeSelectorLayout {
            button_width: usable,
            stacked: true,
        }
    }
}

/// Width one selector button needs for a side-by-side pair to still read:
/// enough to show the LONGER of the two mode labels IN FULL (never elided),
/// plus the button's own horizontal padding.
///
/// Measured against the real font/text-scale (and the real localized labels,
/// so Danish "Avanceret" is accounted for) rather than a magic constant. The
/// longer label is the right yardstick, not the shorter one (Opus review,
/// 2026-09-16, fixing the inverted `f32::min` fold this replaced): folding on
/// the shortest label measures how much room "Simple"/"Enkel" needs and says
/// nothing about whether "Advanced"/"Avanceret" — the label that actually
/// gets clipped — fits, so the old fold let a pair report "side by side" at a
/// width where the longer label silently elided. Below THIS width neither
/// half can show its full label and stacking (full panel width per row) is
/// the only readable option.
fn min_readable_mode_button_width(ui: &egui::Ui, raw_language: &str) -> f32 {
    let longest_label = SettingsMode::ALL
        .into_iter()
        .map(|mode| {
            egui::WidgetText::from(mode.label(raw_language))
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Button,
                )
                .size()
                .x
        })
        .fold(0.0, f32::max);
    longest_label + 2.0 * ui.spacing().button_padding.x
}

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
    ///
    /// Sizing comes from [`mode_selector_layout`], i.e. from the panel's real
    /// available width minus the two 1 px strokes, and each button truncates
    /// its label. In a narrow window the pair stacks into two full-width rows
    /// instead of overflowing the sidebar (#895).
    pub(in crate::ui) fn settings_mode_selector(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let current = SettingsMode::from_raw(&self.settings.ui_settings_mode);
        let language = self.settings.ui_language.clone();
        let help = ui_text(&language, UiTextKey::SettingsModeHelp);
        // The sidebar panel is now sized to fit this selector (see
        // `sidebar_width::sidebar_content_width`), so at every normal window
        // size `layout.stacked` is false and both buttons show their full
        // label with the theme's REAL `button_padding.x` — no shrunk
        // padding here (an earlier draft did that instead of fixing the
        // panel width; Opus review, 2026-09-16). The stacked fallback below
        // still exists for the one case the content-driven width does NOT
        // cover: a window too narrow for the sidebar's content-driven width
        // to fit under its window-fraction cap.
        let layout = mode_selector_layout(
            ui.available_width(),
            min_readable_mode_button_width(ui, &language),
        );
        // Collect the click instead of calling `set_settings_mode` inline:
        // the button loop runs inside a closure that already borrows `self`
        // immutably for the labels, and it is shared by both the stacked and
        // side-by-side branches.
        let mut clicked_mode = None;
        let mut add_mode_buttons = |ui: &mut egui::Ui| {
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
                        egui::vec2(layout.button_width, MODE_BUTTON_HEIGHT),
                        egui::Button::new(text)
                            // Without a wrap mode the label is clipped by the
                            // PANEL (sliced mid-word); truncate elides it
                            // inside the button instead (#895) — the safety
                            // net for the rare window too narrow for the
                            // content-driven sidebar width to fit under its cap.
                            .truncate()
                            .fill(fill)
                            .stroke(egui::Stroke::new(MODE_BUTTON_STROKE, palette.border_soft)),
                    )
                    .on_hover_text(help)
                    .clicked();
                if clicked && !selected {
                    clicked_mode = Some(mode);
                }
            }
        };
        if layout.stacked {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                add_mode_buttons(ui);
            });
        } else {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                add_mode_buttons(ui);
            });
        }
        if let Some(mode) = clicked_mode {
            self.set_settings_mode(mode);
        }
        ui.add_space(10.0);
    }

    /// Apply and persist the Simple/Advanced settings-visibility preference.
    /// Mirrors `set_log_view`'s instant-apply intent (the sidebar toggle
    /// applies immediately and never commits the user's other pending edits
    /// — those stay in `settings` until an explicit Save) but persists
    /// differently: it writes ONLY the `ui_settings_mode` key through
    /// [`config::set_raw_string_key`] — a genuine raw JSON read/insert/write
    /// that touches nothing else in the file — rather than resaving the
    /// whole cached `saved_settings` snapshot. That snapshot can be stale
    /// the moment a concurrent `wd config set` (or a hand-edited
    /// config.json) has changed some OTHER key on disk since this session
    /// last loaded; resaving it wholesale would silently revert that
    /// external edit (Codex P2). The in-memory `saved_settings.ui_settings_mode`
    /// is updated only AFTER a successful write, so a failed save leaves
    /// `has_unsaved_settings` correctly reporting the mode as still pending
    /// instead of looking clean (Codex P2).
    ///
    /// Deliberately does NOT use [`config::set_value`] (Codex P1): that
    /// path merges the new key into a full `AppSettings` snapshot and then
    /// serializes every OTHER known setting's typed value too — for a
    /// sparse or missing config.json, that materializes each one's schema
    /// default into the file. Config takes precedence over environment at
    /// load time, so a user relying on an env-only override (e.g.
    /// `VOICEPI_LOCAL_ONLY=1`) would have it silently and permanently
    /// clobbered by `local_only = false` the moment they merely clicked
    /// this toggle — a casual, frequent, non-configuration UI action, not
    /// an explicit single-key request. [`config::set_raw_string_key`]
    /// writes only `ui_settings_mode`, leaving every other key (known or
    /// unknown to this app) byte-for-value untouched.
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
        match config::set_raw_string_key("ui_settings_mode", mode.id(), &config::config_path()) {
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
    /// It is therefore merged in by hand, under the SAME visibility filter
    /// as every other key: `post_api_key` is allow-listed into Simple mode
    /// (the Post tab itself is now shown there), so a pending edit to it is
    /// on screen and must NOT be reported as hidden.
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
        if self.post_api_key_input != self.saved_post_api_key_input
            && !setting_visible(mode, "post_api_key")
        {
            keys.insert("post_api_key".to_owned());
        }
        keys.into_iter().collect()
    }
}
