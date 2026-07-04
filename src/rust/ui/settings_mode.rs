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
    fn visible_tabs_preserves_declaration_order() {
        // The sidebar order must stay identical to the canonical Tab::ALL
        // order; Simple mode drops entries but never reorders them.
        let simple: Vec<Tab> = visible_tabs(SettingsMode::Simple).collect();
        assert_eq!(simple, vec![Tab::Log, Tab::Speech]);

        let advanced: Vec<Tab> = visible_tabs(SettingsMode::Advanced).collect();
        assert_eq!(advanced.as_slice(), Tab::ALL.as_slice());
    }
}
