//! Headless-rendering proof that `system_tab` actually applies the
//! Simple/Advanced gating — see `render_test_support` for the technique.

use super::super::render_test_support::{contains_text, rendered_texts_for_tab};
use super::super::test_support::test_app;
use super::*;

fn app_in_mode(mode: SettingsMode) -> WhisperDictateApp {
    let mut app = test_app(AppSettings {
        ui_settings_mode: mode.id().to_owned(),
        ..AppSettings::default()
    });
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app
}

#[test]
fn simple_mode_renders_only_appearance() {
    let mut app = app_in_mode(SettingsMode::Simple);
    let texts = rendered_texts_for_tab(&mut app, Tab::System);

    assert!(contains_text(&texts, "UI theme"));
    assert!(contains_text(&texts, "UI language"));
    for hidden in [
        "Reload config",
        "Doctor",
        "Run benchmark",
        "Dictation view",
        "UI text scale",
        "Feedback sounds",
        "Check for updates",
        "Diagnostics",
        "JSON stdout",
        "Metrics JSONL",
        "Local only",
    ] {
        assert!(
            !contains_text(&texts, hidden),
            "'{hidden}' must be hidden in Simple mode, got: {texts:?}"
        );
    }
}

#[test]
fn advanced_mode_renders_every_system_section() {
    let mut app = app_in_mode(SettingsMode::Advanced);
    let texts = rendered_texts_for_tab(&mut app, Tab::System);

    for visible in [
        "UI theme",
        "UI language",
        "Reload config",
        "Doctor",
        "Run benchmark",
        "Dictation view",
        "UI text scale",
        "Feedback sounds",
        "Check for updates",
        "Diagnostics",
        "JSON stdout",
        "Metrics JSONL",
        "Local only",
    ] {
        assert!(
            contains_text(&texts, visible),
            "'{visible}' must be visible in Advanced mode, got: {texts:?}"
        );
    }
}

/// Codex P1: switching to Simple while a corpus-recording batch is running
/// must not hide the only Stop Batch button — the recorder keeps running in
/// the background regardless of which settings page is showing.
#[test]
fn simple_mode_keeps_the_active_corpus_batch_panel_reachable() {
    let mut app = app_in_mode(SettingsMode::Simple);
    app.corpus_loaded = true;
    app.corpus_items = vec![CorpusItem {
        id: "item-1".to_owned(),
        text: "Read this aloud".to_owned(),
        language: "en".to_owned(),
    }];
    app.corpus_batch = CorpusBatch::new(vec!["item-1".to_owned()]);
    assert!(app.corpus_batch_active());

    let texts = rendered_texts_for_tab(&mut app, Tab::System);

    assert!(
        contains_text(&texts, "Item 1 of 1"),
        "expected the batch progress line, got: {texts:?}"
    );
    // Every other Maintenance action stays hidden — only the batch panel
    // (with its Stop button) is allowed through in Simple mode. ("Run
    // benchmark" is deliberately not checked here: the corpus panel's own
    // purpose blurb cross-references "System → Run benchmark" in prose, so
    // that substring is expected to appear even with the button gone.)
    for hidden in ["Reload config", "Doctor"] {
        assert!(
            !contains_text(&texts, hidden),
            "'{hidden}' must stay hidden in Simple mode even mid-batch, got: {texts:?}"
        );
    }
}

#[test]
fn simple_mode_hides_the_maintenance_cluster_with_no_active_batch() {
    let mut app = app_in_mode(SettingsMode::Simple);
    assert!(!app.corpus_batch_active());

    let texts = rendered_texts_for_tab(&mut app, Tab::System);

    assert!(!contains_text(&texts, "Item 1 of"));
}
