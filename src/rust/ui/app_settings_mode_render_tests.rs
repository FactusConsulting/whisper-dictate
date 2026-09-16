//! Headless-render proof of the `fn ui()` render-time `selected_tab` clamp
//! (app.rs). Split out of `app_tests.rs` (Codex P2: that file was already
//! 703 lines; adding these here keeps it at its pre-existing size) rather
//! than folded into `settings_mode_navigation_tests.rs`, since these
//! specifically need the real render harness (`render_test_support`) to
//! drive `eframe::App::ui`, not just the pure `fallback_tab_for_mode` rule.

use super::render_test_support::rendered_texts;
use super::test_support::test_app;
use super::{AppSettings, SettingsMode, Tab};

#[test]
fn a_real_render_pass_clamps_a_hidden_selected_tab_to_speech() {
    // A hand-edited config.json (or a `wd config set ui_settings_mode
    // simple` run from another terminal, applied on the next `reload`) can
    // put the app in Simple mode while `selected_tab` still points at a
    // hidden Advanced-only tab. `select_tab`/`reload_settings` cover the
    // paths that change the mode from inside the app; this proves the
    // `fn ui()` render-time clamp is the backstop for every other path,
    // driving the REAL render method (not just calling the pure
    // `fallback_tab_for_mode` rule directly).
    let mut app = test_app(AppSettings {
        ui_settings_mode: "simple".to_owned(),
        ..AppSettings::default()
    });
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app.selected_tab = Tab::Quality;

    let _ = rendered_texts(&mut app);

    assert_eq!(app.selected_tab, Tab::Speech);
}

#[test]
fn a_real_render_pass_leaves_a_visible_selected_tab_alone() {
    let mut app = test_app(AppSettings {
        ui_settings_mode: "simple".to_owned(),
        ..AppSettings::default()
    });
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app.selected_tab = Tab::Output;

    let _ = rendered_texts(&mut app);

    assert_eq!(app.selected_tab, Tab::Output);
}

#[test]
fn a_real_render_pass_never_clamps_in_advanced_mode() {
    let mut app = test_app(AppSettings {
        ui_settings_mode: SettingsMode::Advanced.id().to_owned(),
        ..AppSettings::default()
    });
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app.selected_tab = Tab::Quality;

    let _ = rendered_texts(&mut app);

    assert_eq!(app.selected_tab, Tab::Quality);
}
