//! Headless-rendering proof that `output_tab` actually applies the
//! Simple/Advanced gating (not just that `setting_visible` is
//! self-consistent) — see `render_test_support` for the technique.

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
fn simple_mode_renders_only_inject_mode() {
    let mut app = app_in_mode(SettingsMode::Simple);
    let texts = rendered_texts_for_tab(&mut app, Tab::Output);

    assert!(contains_text(&texts, "Inject mode"));
    for hidden in [
        "Format commands",
        "Command hook",
        "History enabled",
        "History JSONL",
        "Preview history",
        "Open history",
    ] {
        assert!(
            !contains_text(&texts, hidden),
            "'{hidden}' must be hidden in Simple mode, got: {texts:?}"
        );
    }
}

#[test]
fn advanced_mode_renders_every_output_row() {
    let mut app = app_in_mode(SettingsMode::Advanced);
    let texts = rendered_texts_for_tab(&mut app, Tab::Output);

    for visible in [
        "Inject mode",
        "Format commands",
        "Command hook",
        "History enabled",
        "History JSONL",
        "Preview history",
        "Open history",
    ] {
        assert!(
            contains_text(&texts, visible),
            "'{visible}' must be visible in Advanced mode, got: {texts:?}"
        );
    }
}
