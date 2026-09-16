//! Unit tests for the sidebar recording indicator helper, plus the #895
//! narrow-window layout regression tests that drive the real sidebar render.

use super::super::*; // crate::ui::* — UiTextKey, ui_text, ui_palette, RuntimeState, …
use super::recording_indicator_style;
use super::shell_indicator::RecordingIndicatorColor;

/// #895 regression tests. The sidebar panel is now sized to its own content
/// (`sidebar_width::sidebar_content_width`) rather than a fixed function of
/// `ui_text_scale` alone, so the "does nothing elide at a normal window
/// size" and "does the fallback engage once the window is genuinely too
/// narrow" proofs live in `sidebar_width_tests.rs`, which sweeps every
/// scale/language/mode against the REAL production width. What is left here
/// is `settings_mode_selector`'s OWN fallback behaviour given an ARBITRARY
/// narrow panel it did not choose the size of — a distinct concern from "how
/// wide does production make the panel".
mod narrow_sidebar_895 {
    use super::super::super::render_test_support::measure_in_panel;
    use super::super::super::test_support::test_app;
    use super::super::super::{egui, sidebar_width, ui_palette, AppSettings, WhisperDictateApp};

    /// `Margin::symmetric(14, 14)` on the sidebar panel's frame (`ui/app.rs`).
    const SIDEBAR_INNER_MARGIN: f32 = 14.0;

    fn narrow_app(scale: &str) -> WhisperDictateApp {
        let mut app = test_app(AppSettings {
            ui_text_scale: scale.to_owned(),
            ..AppSettings::default()
        });
        app.audio_devices_loaded = true;
        app.settings.update_check = false;
        app.tray.disable();
        app
    }

    #[test]
    fn the_mode_selector_fits_the_panel_it_is_given() {
        // The panel budget at the smallest selectable text scale (0.85):
        // 164 * 0.85 - 2 * 14 = 111 px. The old
        // `(available_width() / 2.0).max(60.0)` ignored that entirely and
        // asked for 2 * 60 + 2 px of stroke = 122 px, so the Advanced button
        // was sliced at the panel edge. Production no longer hands the
        // selector a panel this narrow (see `sidebar_width_tests.rs`), but
        // the selector itself must still degrade gracefully if it is EVER
        // given less room than it needs — this drives it directly with a
        // synthetic narrow panel rather than going through the real
        // content-driven sizing.
        let mut app = narrow_app("0.85");
        let palette = ui_palette(&app.settings.ui_theme);
        let panel = egui::vec2(sidebar_width("0.85") - 2.0 * SIDEBAR_INNER_MARGIN, 400.0);

        let used = measure_in_panel(panel, |ui| app.settings_mode_selector(ui, palette));

        assert!(
            used.width() <= panel.x,
            "the selector occupied {} px inside a {} px panel",
            used.width(),
            panel.x
        );
    }
}

#[test]
fn recording_overrides_running_state() {
    let (key, slot) = recording_indicator_style(Some("recording"), RuntimeState::Running);
    assert_eq!(key, UiTextKey::Recording);
    assert_eq!(slot, RecordingIndicatorColor::Error);
}

#[test]
fn recording_overrides_starting_state() {
    // The live transitional case: worker still Starting but already recording.
    let (key, slot) = recording_indicator_style(Some("recording"), RuntimeState::Starting);
    assert_eq!(key, UiTextKey::Recording);
    assert_eq!(slot, RecordingIndicatorColor::Error);
}

#[test]
fn stopped_runtime_shows_stopped_even_if_pipeline_stale() {
    // Defense-in-depth: a Stopped worker can never legitimately be recording, so
    // a stale `Some("recording")` pipeline stage (e.g. the worker exited or was
    // stopped mid-dictation before its stage was cleared) must NOT light up the
    // red Recording indicator — it resolves to the muted Stopped state.
    let (key, slot) = recording_indicator_style(Some("recording"), RuntimeState::Stopped);
    assert_eq!(key, UiTextKey::Stopped);
    assert_eq!(slot, RecordingIndicatorColor::Muted);
}

#[test]
fn running_without_recording_shows_ready_green() {
    let (key, slot) = recording_indicator_style(None, RuntimeState::Running);
    assert_eq!(key, UiTextKey::Ready);
    assert_eq!(slot, RecordingIndicatorColor::Ok);
}

#[test]
fn starting_shows_warn_amber() {
    let (key, slot) = recording_indicator_style(None, RuntimeState::Starting);
    assert_eq!(key, UiTextKey::Starting);
    assert_eq!(slot, RecordingIndicatorColor::Warn);
}

#[test]
fn stopped_shows_muted_grey() {
    let (key, slot) = recording_indicator_style(None, RuntimeState::Stopped);
    assert_eq!(key, UiTextKey::Stopped);
    assert_eq!(slot, RecordingIndicatorColor::Muted);
}

#[test]
fn color_slot_resolves_to_distinct_palette_colors() {
    let palette = ui_palette("dark");
    // Each slot must resolve to a different colour so the indicator is visually
    // distinguishable across all four states.
    let error = RecordingIndicatorColor::Error.resolve(palette);
    let ok = RecordingIndicatorColor::Ok.resolve(palette);
    let warn = RecordingIndicatorColor::Warn.resolve(palette);
    let muted = RecordingIndicatorColor::Muted.resolve(palette);
    assert_ne!(error, ok);
    assert_ne!(error, warn);
    assert_ne!(error, muted);
    assert_ne!(ok, warn);
    assert_ne!(ok, muted);
    assert_ne!(warn, muted);
}

#[test]
fn recording_indicator_translations_present() {
    // The two new keys must have non-empty strings in both languages.
    assert!(!ui_text("en", UiTextKey::Recording).is_empty());
    assert!(!ui_text("da", UiTextKey::Recording).is_empty());
    assert!(!ui_text("en", UiTextKey::Ready).is_empty());
    assert!(!ui_text("da", UiTextKey::Ready).is_empty());
    // Spot-check expected values.
    assert_eq!(ui_text("en", UiTextKey::Recording), "Recording");
    assert_eq!(ui_text("da", UiTextKey::Recording), "Optager");
    assert_eq!(ui_text("en", UiTextKey::Ready), "Ready");
    assert_eq!(ui_text("da", UiTextKey::Ready), "Klar");
}

#[test]
fn update_available_strings_present_in_both_languages() {
    // Every new user-visible update-check string must be localized.
    for key in [
        UiTextKey::UpdateAvailable,
        UiTextKey::UpdateAvailableHover,
        UiTextKey::SystemUpdates,
        UiTextKey::UpdateCheck,
        UiTextKey::UpdateCheckHelp,
        UiTextKey::UpdateCheckInterval,
        UiTextKey::UpdateCheckIntervalHelp,
    ] {
        assert!(!ui_text("en", key).is_empty(), "EN missing for {key:?}");
        assert!(!ui_text("da", key).is_empty(), "DA missing for {key:?}");
    }
    // The privacy guarantee must be spelled out in the toggle help text in both
    // languages: GitHub-only fetch + no data sent.
    let en_help = ui_text("en", UiTextKey::UpdateCheckHelp);
    assert!(en_help.contains("github.io"), "EN help must name github.io");
    assert!(
        en_help.contains("NO data"),
        "EN help must state no data is sent"
    );
    let da_help = ui_text("da", UiTextKey::UpdateCheckHelp);
    assert!(da_help.contains("github.io"), "DA help must name github.io");
    assert!(
        da_help.contains("INGEN data"),
        "DA help must state no data is sent"
    );
}

#[test]
fn upgrade_badge_action_strings_present_in_both_languages() {
    // The new actionable-badge strings (copy-command hover, open-release hover,
    // and the transient "Copied!" confirmation) must be localized EN+DA.
    for key in [
        UiTextKey::UpdateCopyCommandHover,
        UiTextKey::UpdateOpenReleaseHover,
        UiTextKey::UpdateCommandCopied,
    ] {
        assert!(!ui_text("en", key).is_empty(), "EN missing for {key:?}");
        assert!(!ui_text("da", key).is_empty(), "DA missing for {key:?}");
    }
    // Spot-check expected values so a stray edit can't silently swap them.
    assert_eq!(ui_text("en", UiTextKey::UpdateCommandCopied), "Copied!");
    assert_eq!(ui_text("da", UiTextKey::UpdateCommandCopied), "Kopieret!");
}
