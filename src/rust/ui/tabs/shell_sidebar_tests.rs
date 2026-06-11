//! Unit tests for the sidebar recording indicator helper.

use super::super::*; // crate::ui::* — UiTextKey, ui_text, ui_palette, …
use super::recording_indicator_style;
use super::shell_indicator::RecordingIndicatorColor;
use crate::runtime::RuntimeState;

#[test]
fn recording_overrides_running_state() {
    let (key, slot) = recording_indicator_style(Some("recording"), RuntimeState::Running);
    assert_eq!(key, UiTextKey::Recording);
    assert_eq!(slot, RecordingIndicatorColor::Error);
}

#[test]
fn recording_overrides_stopped_state() {
    // Defensive: if the pipeline somehow says "recording" while the worker is
    // Stopped, we still show the red Recording indicator.
    let (key, slot) = recording_indicator_style(Some("recording"), RuntimeState::Stopped);
    assert_eq!(key, UiTextKey::Recording);
    assert_eq!(slot, RecordingIndicatorColor::Error);
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
