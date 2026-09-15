//! Headless-rendering proof that `core_tab` (the Speech tab) actually applies
//! the Simple/Advanced gating — see `render_test_support` for the technique.
//! The pure `stt_base_url_visible` / `local_only_blocks_cloud_note_visible`
//! decision-function tests live inline in `speech_advanced.rs`; these prove
//! the renderer actually calls them.

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
fn simple_mode_hides_advanced_only_speech_rows() {
    let mut app = app_in_mode(SettingsMode::Simple);
    let texts = rendered_texts_for_tab(&mut app, Tab::Speech);

    for hidden in [
        "Device",
        "Toggle mode",
        "Cloud STT API URL",
        "Cloud STT timeout ms",
    ] {
        assert!(
            !contains_text(&texts, hidden),
            "'{hidden}' must be hidden in Simple mode, got: {texts:?}"
        );
    }
}

#[test]
fn simple_mode_still_shows_every_essential_speech_row() {
    let mut app = app_in_mode(SettingsMode::Simple);
    let texts = rendered_texts_for_tab(&mut app, Tab::Speech);

    for visible in [
        "Speech engine",
        "Whisper model",
        "Cloud STT provider",
        "Cloud STT model",
        "Cloud STT API key",
        "Language",
        "Hotkey",
        "Microphone",
    ] {
        assert!(
            contains_text(&texts, visible),
            "'{visible}' must stay visible in Simple mode, got: {texts:?}"
        );
    }
}

#[test]
fn advanced_mode_shows_every_speech_row() {
    let mut app = app_in_mode(SettingsMode::Advanced);
    let texts = rendered_texts_for_tab(&mut app, Tab::Speech);

    for visible in [
        "Device",
        "Toggle mode",
        "Cloud STT API URL",
        "Cloud STT timeout ms",
    ] {
        assert!(
            contains_text(&texts, visible),
            "'{visible}' must be visible in Advanced mode, got: {texts:?}"
        );
    }
}

#[test]
fn custom_provider_keeps_the_api_url_row_visible_even_in_simple() {
    let mut app = app_in_mode(SettingsMode::Simple);
    app.settings.stt_backend = "openai".to_owned();
    app.settings.stt_provider = "custom".to_owned();

    let texts = rendered_texts_for_tab(&mut app, Tab::Speech);

    assert!(
        contains_text(&texts, "Cloud STT API URL"),
        "Custom provider must keep the API URL row visible in Simple mode, got: {texts:?}"
    );
}

#[test]
fn openai_provider_hides_the_api_url_row_in_simple() {
    let mut app = app_in_mode(SettingsMode::Simple);
    app.settings.stt_backend = "openai".to_owned();
    app.settings.stt_provider = "openai".to_owned();

    let texts = rendered_texts_for_tab(&mut app, Tab::Speech);

    assert!(
        !contains_text(&texts, "Cloud STT API URL"),
        "OpenAI provider must hide the API URL row in Simple mode, got: {texts:?}"
    );
}

#[test]
fn local_only_note_renders_in_simple_when_cloud_is_blocked() {
    let mut app = app_in_mode(SettingsMode::Simple);
    app.settings.stt_backend = "openai".to_owned();
    app.settings.local_only = true;

    let texts = rendered_texts_for_tab(&mut app, Tab::Speech);

    assert!(
        contains_text(&texts, "Local-only mode blocks cloud speech recognition"),
        "expected the local-only note, got: {texts:?}"
    );
}

#[test]
fn local_only_note_absent_when_cloud_is_not_blocked() {
    let mut app = app_in_mode(SettingsMode::Simple);
    app.settings.stt_backend = "openai".to_owned();
    app.settings.local_only = false;

    let texts = rendered_texts_for_tab(&mut app, Tab::Speech);

    assert!(!contains_text(
        &texts,
        "Local-only mode blocks cloud speech recognition"
    ));
}
