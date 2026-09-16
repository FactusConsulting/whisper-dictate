//! Headless-rendering proof that `core_tab` (the Speech tab) actually applies
//! the Simple/Advanced gating — see `render_test_support` for the technique.
//! The pure `stt_base_url_visible` / `local_only_blocks_cloud_note_visible`
//! decision-function tests live inline in `speech_advanced.rs`; these prove
//! the renderer actually calls them.

use super::super::render_test_support::{contains_text, rendered_texts_for_tab};
use super::super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::*;

fn app_in_mode(mode: SettingsMode) -> WhisperDictateApp {
    app_in_mode_with_settings(mode, |_| {})
}

/// Like [`app_in_mode`], but lets the caller finish building the
/// `AppSettings` BEFORE it's handed to `test_app` — which clones it into
/// both `settings` AND `saved_settings`. Needed for
/// `desired_local_only`-gated coverage: that helper reads
/// `saved_settings.local_only`, so mutating `app.settings.local_only` on an
/// already-built app (the pattern every other test in this file uses) would
/// silently NOT exercise the "saved setting blocks cloud" case.
fn app_in_mode_with_settings(
    mode: SettingsMode,
    build: impl FnOnce(&mut AppSettings),
) -> WhisperDictateApp {
    let mut settings = AppSettings {
        ui_settings_mode: mode.id().to_owned(),
        ..AppSettings::default()
    };
    build(&mut settings);
    let mut app = test_app(settings);
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

/// The "config-set" path: `local_only` came from a saved setting (present
/// in both `settings` and `saved_settings`, as it would be after loading a
/// config.json with `"local_only":true`, or after a prior Save). The note
/// must render AND point at the real remedy (Advanced -> System ->
/// Integration).
#[test]
fn local_only_note_renders_in_simple_when_the_saved_setting_blocks_cloud() {
    let mut app = app_in_mode_with_settings(SettingsMode::Simple, |settings| {
        settings.stt_backend = "openai".to_owned();
        settings.local_only = true;
    });

    let texts = rendered_texts_for_tab(&mut app, Tab::Speech);

    assert!(
        contains_text(&texts, "Local-only mode blocks cloud speech recognition"),
        "expected the local-only note, got: {texts:?}"
    );
    assert!(
        contains_text(&texts, "System"),
        "a setting-sourced block should point at System -> Integration, got: {texts:?}"
    );
}

/// Codex P1 follow-up: the "ambient environment" path. No `local_only` key
/// on disk at all (`settings.local_only` and `saved_settings.local_only`
/// both stay `false`), but `VOICEPI_LOCAL_ONLY=1` is set in the real
/// process environment -- `desired_local_only()` (and the runtime) already
/// treat cloud STT as blocked here, so the note must still render, worded
/// for the environment rather than the (in this case unreachable AND
/// useless) Settings toggle.
#[test]
fn local_only_note_renders_in_simple_when_an_ambient_env_override_blocks_cloud() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::set("VOICEPI_LOCAL_ONLY", "1");

    let mut app = app_in_mode(SettingsMode::Simple);
    app.settings.stt_backend = "openai".to_owned();
    assert!(!app.settings.local_only);
    assert!(!app.saved_settings.local_only);

    let texts = rendered_texts_for_tab(&mut app, Tab::Speech);

    assert!(
        contains_text(&texts, "Local-only mode blocks cloud speech recognition"),
        "expected the local-only note, got: {texts:?}"
    );
    assert!(
        contains_text(&texts, "VOICEPI_LOCAL_ONLY"),
        "an env-sourced block must name VOICEPI_LOCAL_ONLY rather than point \
         at the hidden (and here useless) Settings toggle, got: {texts:?}"
    );
}

/// CI-caught flake (`cargo test --tests`, where every test shares one
/// process, unlike `nextest`'s per-test process): the note's visibility
/// depends on `desired_local_only()`, which reads the REAL
/// `VOICEPI_LOCAL_ONLY` process env var — so without its own lock, this
/// test could observe the ambient-override test above mid-mutation on
/// another thread and see the note when it doesn't expect one. Hold the
/// same crate-wide `ENV_TEST_LOCK` every env-touching test in this repo
/// uses (see `test_env_lock`'s module docs) and explicitly force the var
/// unset, rather than merely hoping the host environment doesn't have it
/// set.
#[test]
fn local_only_note_absent_when_cloud_is_not_blocked() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let _env_guard = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");

    let mut app = app_in_mode(SettingsMode::Simple);
    app.settings.stt_backend = "openai".to_owned();
    app.settings.local_only = false;

    let texts = rendered_texts_for_tab(&mut app, Tab::Speech);

    assert!(!contains_text(
        &texts,
        "Local-only mode blocks cloud speech recognition"
    ));
}
