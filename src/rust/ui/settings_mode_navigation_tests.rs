//! Tab-selection-fallback tests for `select_tab`, `set_settings_mode`, and
//! `reload_settings`. Split out of `settings_mode_tests.rs` (Codex P2: that
//! file had grown to 563 lines) — see its doc comment for the full module
//! breakdown.

use super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::*;

#[test]
fn switching_to_simple_falls_back_the_selected_tab_when_it_becomes_hidden() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    app.selected_tab = Tab::Quality;

    app.set_settings_mode(SettingsMode::Simple);

    assert_eq!(app.selected_tab, Tab::Speech);
}

#[test]
fn switching_to_simple_keeps_the_selected_tab_when_it_stays_visible() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    app.selected_tab = Tab::Output;

    app.set_settings_mode(SettingsMode::Simple);

    assert_eq!(app.selected_tab, Tab::Output);
}

#[test]
fn switching_to_advanced_never_changes_the_selected_tab() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    app.selected_tab = Tab::Quality;

    app.set_settings_mode(SettingsMode::Advanced);

    assert_eq!(app.selected_tab, Tab::Quality);
}

#[test]
fn select_tab_clamps_a_hidden_tab_in_simple_mode() {
    let mut app = test_app(AppSettings {
        ui_settings_mode: "simple".to_owned(),
        ..AppSettings::default()
    });

    app.select_tab(Tab::Dictionary);

    assert_eq!(app.selected_tab, Tab::Speech);
}

#[test]
fn select_tab_keeps_a_visible_tab_in_simple_mode() {
    let mut app = test_app(AppSettings {
        ui_settings_mode: "simple".to_owned(),
        ..AppSettings::default()
    });

    app.select_tab(Tab::Output);

    assert_eq!(app.selected_tab, Tab::Output);
}

#[test]
fn select_tab_never_clamps_in_advanced_mode() {
    let mut app = test_app(AppSettings::default());

    app.select_tab(Tab::Quality);

    assert_eq!(app.selected_tab, Tab::Quality);
}

#[test]
fn reload_settings_falls_back_the_selected_tab_when_the_reloaded_config_is_simple() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(&config, r#"{"ui_settings_mode":"simple"}"#).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    // Started on an Advanced-only tab under an Advanced-mode fixture (the
    // scenario a hand-edited config.json, or a `wd config set
    // ui_settings_mode simple` run from another terminal, produces: the app
    // never called `set_settings_mode` itself).
    let mut app = test_app(AppSettings::default());
    app.selected_tab = Tab::Quality;

    app.reload_settings();

    assert_eq!(app.settings.ui_settings_mode, "simple");
    assert_eq!(app.selected_tab, Tab::Speech);
}
