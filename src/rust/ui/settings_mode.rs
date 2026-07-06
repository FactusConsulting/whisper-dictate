//! Simple / Advanced settings mode (Issue #334).
//!
//! The Settings surface has grown to ~60 controls across 7 tabs. A first-time
//! user only needs mic + hotkey + backend to make the app work; the rest is
//! power-user territory. The [`SettingsMode`] toggle hides the niche knobs by
//! default, and every settings-row descriptor carries a `simple` bool so the
//! renderer's visibility predicate is a single, testable function.
//!
//! Persistence lives in [`crate::config::AppSettings::settings_mode`]. Load
//! migration in `crate::config::load` upgrades an established user's implicit
//! default from `Simple` to `Advanced` so nothing they already configured
//! disappears; a genuinely-fresh install keeps `Simple`.

use super::Tab;

/// Which of the two settings faces the user is looking at. Persisted through
/// [`AppSettings::settings_mode`](crate::config::AppSettings::settings_mode)
/// as `"simple"` / `"advanced"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum SettingsMode {
    /// Slim view: only the fields a user needs to configure to make the app
    /// work (mic, hotkey, backend/model, language, api key). All other rows
    /// are hidden.
    Simple,
    /// Full view: every existing knob is visible — matches the pre-#334
    /// behaviour.
    Advanced,
}

impl SettingsMode {
    /// Parse the persisted string form. Anything unrecognised falls back to
    /// [`SettingsMode::Advanced`] so a hand-edited config can never leave a
    /// power user unable to reach their settings.
    pub(in crate::ui) fn from_raw(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "simple" => Self::Simple,
            _ => Self::Advanced,
        }
    }

    /// The canonical serialization used by
    /// [`AppSettings::settings_mode`](crate::config::AppSettings::settings_mode).
    pub(in crate::ui) fn as_raw(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Advanced => "advanced",
        }
    }
}

/// Should a settings-row descriptor be visible in the current mode?
///
/// `is_simple_row` is the row's `simple: bool` attribute from the schema-like
/// row descriptor: `true` marks a "must-see for a new user" row (kept in
/// Simple mode) and `false` marks a power-user knob (hidden in Simple).
///
/// - [`SettingsMode::Advanced`] shows EVERY row — that is the whole point of
///   the toggle: it must be a strict superset so a Simple-mode user can flip
///   back and still see the field they changed.
/// - [`SettingsMode::Simple`] shows ONLY rows marked `simple = true`.
pub(in crate::ui) fn row_visible(mode: SettingsMode, is_simple_row: bool) -> bool {
    match mode {
        SettingsMode::Advanced => true,
        SettingsMode::Simple => is_simple_row,
    }
}

/// Whether a settings [`Tab`] appears in the sidebar for the current mode.
///
/// Simple mode keeps only [`Tab::Log`] (the runtime/dictation view) and
/// [`Tab::Speech`] (which itself hides the advanced-only rows via
/// [`row_visible`]); every other tab is a power-user surface.
pub(in crate::ui) fn tab_visible(mode: SettingsMode, tab: Tab) -> bool {
    match mode {
        SettingsMode::Advanced => true,
        SettingsMode::Simple => matches!(tab, Tab::Log | Tab::Speech),
    }
}

/// Iterator over the tabs visible in the sidebar for `mode`, preserving the
/// canonical [`Tab::ALL`] order. Used by the sidebar to render one nav button
/// per visible tab.
pub(in crate::ui) fn visible_tabs(mode: SettingsMode) -> impl Iterator<Item = Tab> {
    Tab::ALL
        .into_iter()
        .filter(move |tab| tab_visible(mode, *tab))
}

/// Snap a possibly-hidden tab back into the visible-set for `mode`. The
/// sidebar only renders visible tabs, but the central panel and Reset Page
/// still dispatch off `WhisperDictateApp::selected_tab`. If a config
/// reload flips the mode to Simple while the user was parked on an
/// Advanced-only tab (e.g. Quality/Post/System), the sidebar would hide
/// the entry but the central panel would still render the hidden page —
/// and Reset Page would target it. Normalizing on every `ui()` frame
/// (Codex #435 P2) is a single defensive line that guarantees Simple mode
/// is actually enforced across every render surface.
///
/// The snap target is [`Tab::Speech`] — that is the tab the mode toggle
/// itself falls back to when Simple hides the current tab, so keeping the
/// two sites in sync avoids two different "safe" tabs for the same
/// condition.
pub(in crate::ui) fn normalize_selected_tab(mode: SettingsMode, tab: Tab) -> Tab {
    if tab_visible(mode, tab) {
        tab
    } else {
        Tab::Speech
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_accepts_canonical_values_and_defaults_to_advanced() {
        // Canonical values round-trip.
        assert_eq!(SettingsMode::from_raw("simple"), SettingsMode::Simple);
        assert_eq!(SettingsMode::from_raw("advanced"), SettingsMode::Advanced);
        // Case-insensitive + whitespace tolerant — the setting is edited
        // through the UI toggle in practice but a hand-edited config
        // shouldn't lock the user out.
        assert_eq!(SettingsMode::from_raw("Simple"), SettingsMode::Simple);
        assert_eq!(
            SettingsMode::from_raw("  ADVANCED "),
            SettingsMode::Advanced
        );
        // Anything unrecognised falls back to Advanced so a garbled value
        // never hides a power-user's fields.
        assert_eq!(SettingsMode::from_raw(""), SettingsMode::Advanced);
        assert_eq!(SettingsMode::from_raw("garbage"), SettingsMode::Advanced);
    }

    #[test]
    fn as_raw_round_trips_through_from_raw() {
        for mode in [SettingsMode::Simple, SettingsMode::Advanced] {
            assert_eq!(SettingsMode::from_raw(mode.as_raw()), mode);
        }
    }

    #[test]
    fn row_visible_advanced_shows_everything() {
        // Advanced is a strict superset: EVERY row is visible regardless
        // of the row's simple flag.
        assert!(row_visible(SettingsMode::Advanced, true));
        assert!(row_visible(SettingsMode::Advanced, false));
    }

    #[test]
    fn row_visible_simple_shows_only_simple_rows() {
        // Simple mode hides everything except the essential rows.
        assert!(row_visible(SettingsMode::Simple, true));
        assert!(!row_visible(SettingsMode::Simple, false));
    }

    #[test]
    fn tab_visible_simple_hides_power_user_tabs() {
        // Log + Speech are the essentials; everything else is power-user
        // territory and stays hidden in Simple mode.
        assert!(tab_visible(SettingsMode::Simple, Tab::Log));
        assert!(tab_visible(SettingsMode::Simple, Tab::Speech));
        assert!(!tab_visible(SettingsMode::Simple, Tab::Quality));
        assert!(!tab_visible(SettingsMode::Simple, Tab::Dictionary));
        assert!(!tab_visible(SettingsMode::Simple, Tab::Output));
        assert!(!tab_visible(SettingsMode::Simple, Tab::Post));
        assert!(!tab_visible(SettingsMode::Simple, Tab::Profiles));
        assert!(!tab_visible(SettingsMode::Simple, Tab::System));
    }

    #[test]
    fn tab_visible_advanced_shows_every_tab() {
        for tab in Tab::ALL {
            assert!(
                tab_visible(SettingsMode::Advanced, tab),
                "tab {tab:?} must be visible in advanced mode"
            );
        }
    }

    #[test]
    fn normalize_selected_tab_keeps_visible_tabs_untouched() {
        // A visible tab must round-trip through the normalizer unchanged so
        // a config reload that leaves the user on Speech/Log doesn't
        // spuriously nudge them onto the fallback.
        for tab in Tab::ALL {
            assert_eq!(
                normalize_selected_tab(SettingsMode::Advanced, tab),
                tab,
                "advanced-mode {tab:?} must not be normalized",
            );
        }
        assert_eq!(
            normalize_selected_tab(SettingsMode::Simple, Tab::Log),
            Tab::Log,
        );
        assert_eq!(
            normalize_selected_tab(SettingsMode::Simple, Tab::Speech),
            Tab::Speech,
        );
    }

    #[test]
    fn normalize_selected_tab_snaps_hidden_tabs_to_speech_in_simple_mode() {
        // Codex #435 P2: if the user was parked on Quality/Post/System/…
        // when a config reload flipped Simple mode on, snap them onto
        // Speech so both sidebar filter and central panel dispatch agree
        // on which tab is being shown. Speech (not Log) matches the
        // fallback the mode-toggle button already uses.
        for tab in [
            Tab::Quality,
            Tab::Dictionary,
            Tab::Output,
            Tab::Post,
            Tab::Profiles,
            Tab::System,
        ] {
            assert_eq!(
                normalize_selected_tab(SettingsMode::Simple, tab),
                Tab::Speech,
                "hidden {tab:?} must snap to Speech in Simple mode",
            );
        }
    }

    /// Codex #435 P2 / AGENTS.md Windows-first: this compile-time-gated
    /// smoke exercises the Simple/Advanced toggle logic on the Windows CI
    /// runner. It does NOT open an egui context (the eframe smoke elsewhere
    /// is the render harness); it pins that on Windows the mode toggle
    /// preserves the exact tab-visibility contract and does not diverge
    /// from the non-Windows path — Windows-specific `cfg!(windows)` gates
    /// live in speech.rs (xkb layout row), and this test guards against a
    /// future Windows-only branch that would let a Simple-mode Windows
    /// user see (or lose) a tab the other platforms hide (or keep).
    #[cfg(windows)]
    #[test]
    fn windows_settings_mode_toggle_contract_matches_other_platforms() {
        // The tab-visibility predicate is compiled identically on Windows
        // and non-Windows targets; this test runs on the Windows CI matrix
        // so a Windows-only regression would surface here rather than in
        // manual smoke.
        for tab in Tab::ALL {
            assert!(
                tab_visible(SettingsMode::Advanced, tab),
                "windows: advanced-mode must show {tab:?}",
            );
        }
        // Only Log + Speech survive Simple mode on Windows too.
        let visible: Vec<Tab> = visible_tabs(SettingsMode::Simple).collect();
        assert_eq!(visible, vec![Tab::Log, Tab::Speech]);
        // The row-visibility helper stays a pure superset check on Windows.
        assert!(row_visible(SettingsMode::Advanced, false));
        assert!(row_visible(SettingsMode::Advanced, true));
        assert!(!row_visible(SettingsMode::Simple, false));
        assert!(row_visible(SettingsMode::Simple, true));
        // normalize_selected_tab snaps hidden tabs to Speech (not Log) on
        // Windows too — same fallback the mode-toggle button uses.
        assert_eq!(
            normalize_selected_tab(SettingsMode::Simple, Tab::System),
            Tab::Speech,
        );
    }

    #[test]
    fn visible_tabs_preserves_declaration_order() {
        // The sidebar order must stay identical to the canonical Tab::ALL
        // order; Simple mode drops entries but never reorders them.
        let simple: Vec<Tab> = visible_tabs(SettingsMode::Simple).collect();
        assert_eq!(simple, vec![Tab::Log, Tab::Speech]);

        let advanced: Vec<Tab> = visible_tabs(SettingsMode::Advanced).collect();
        assert_eq!(advanced.as_slice(), Tab::ALL.as_slice());
    }
}
