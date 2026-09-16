//! Persistence / dirty-tracking / hidden-pending-edit tests for
//! `set_settings_mode` and the config-load default. Split out of
//! `settings_mode_tests.rs` (Codex P2: that file had grown to 563 lines) —
//! see its doc comment for the full module breakdown.

use super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::*;

#[test]
fn missing_config_key_loads_as_advanced() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(&config, "{}").unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let loaded = config::load_settings().unwrap();
    assert_eq!(loaded.ui_settings_mode, "advanced");
}

#[test]
fn toggling_settings_mode_persists_immediately_without_marking_settings_dirty() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    assert!(!app.has_unsaved_settings());

    app.set_settings_mode(SettingsMode::Simple);

    assert_eq!(app.settings.ui_settings_mode, "simple");
    assert_eq!(app.saved_settings.ui_settings_mode, "simple");
    assert!(
        !app.has_unsaved_settings(),
        "toggling settings mode must not mark settings dirty"
    );

    // The preference is written through to disk so it survives a restart.
    let on_disk = config::AppSettings::from_value(
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(on_disk.ui_settings_mode, "simple");
}

#[test]
fn toggling_settings_mode_leaves_unrelated_pending_edits_uncommitted() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    // A genuine unsaved edit the user has not chosen to save yet.
    app.settings.max_chars_per_second = "9".to_owned();
    assert!(app.has_unsaved_settings());

    app.set_settings_mode(SettingsMode::Simple);

    assert!(app.has_unsaved_settings());
    let on_disk = config::AppSettings::from_value(
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(on_disk.ui_settings_mode, "simple");
    assert_eq!(
        on_disk.max_chars_per_second,
        AppSettings::default().max_chars_per_second
    );
}

#[test]
fn hidden_pending_edit_keys_is_empty_in_advanced_mode() {
    let mut app = test_app(AppSettings::default());
    app.settings.device = "cpu".to_owned();

    assert!(app.hidden_pending_edit_keys().is_empty());
}

#[test]
fn hidden_pending_edit_keys_is_empty_with_no_pending_edits() {
    let app = test_app(AppSettings {
        ui_settings_mode: "simple".to_owned(),
        ..AppSettings::default()
    });

    assert!(app.hidden_pending_edit_keys().is_empty());
}

#[test]
fn hidden_pending_edit_keys_finds_an_edit_to_an_advanced_field() {
    let mut app = test_app(AppSettings {
        ui_settings_mode: "simple".to_owned(),
        ..AppSettings::default()
    });
    // `device` is advanced (hidden in Simple) and now differs from the saved
    // snapshot: a genuine pending edit the user can no longer see.
    app.settings.device = "cpu".to_owned();

    let hidden = app.hidden_pending_edit_keys();

    assert_eq!(hidden, vec!["device".to_owned()]);
}

/// Codex: `reset_current_tab_settings` on an advanced-only tab (Quality,
/// entirely hidden in Simple mode — so this can only happen from Advanced)
/// records an explicit nullable-clear intent for a field that was ALREADY
/// an empty string in both `settings` and `saved_settings`. The value
/// itself does not change, so the JSON diff `hidden_pending_edit_keys`
/// otherwise relies on sees nothing — but the explicit-clear intent still
/// means the next Save persists a `null` for that key (suppressing any
/// ambient environment-variable fallback), so it must be surfaced as a
/// hidden pending edit once the user switches to Simple mode.
#[test]
fn hidden_pending_edit_keys_finds_an_explicit_clear_with_no_value_diff() {
    let mut app = test_app(AppSettings::default());
    assert_eq!(app.settings.initial_prompt, "");
    assert_eq!(app.saved_settings.initial_prompt, "");
    app.selected_tab = Tab::Quality;

    // Only reachable from Advanced mode, since Quality is hidden entirely
    // in Simple mode.
    app.reset_current_tab_settings();
    assert_eq!(
        app.settings.initial_prompt, app.saved_settings.initial_prompt,
        "the reset must not itself create a value diff for an already-empty field"
    );

    app.settings.ui_settings_mode = "simple".to_owned();

    let hidden = app.hidden_pending_edit_keys();

    assert!(
        hidden.contains(&"initial_prompt".to_owned()),
        "expected initial_prompt in hidden pending edits, got: {hidden:?}"
    );
}

#[test]
fn hidden_pending_edit_keys_ignores_an_edit_to_an_essential_field() {
    let mut app = test_app(AppSettings {
        ui_settings_mode: "simple".to_owned(),
        ..AppSettings::default()
    });
    // `lang` is essential (visible in Simple), so it is a normal pending edit
    // the user can already see and save — not a "hidden" one.
    app.settings.lang = "da".to_owned();

    assert!(app.hidden_pending_edit_keys().is_empty());
}

/// Codex P2: a pending Post API key edit lives outside the `AppSettings`
/// snapshot (`post_api_key_input` vs `saved_post_api_key_input`, staged for
/// the OS credential store), so it must still be flagged as a hidden pending
/// edit when the entirely-Post-tab-hiding Simple mode is active.
#[test]
fn hidden_pending_edit_keys_finds_a_pending_post_api_key_edit() {
    let mut app = test_app(AppSettings {
        ui_settings_mode: "simple".to_owned(),
        ..AppSettings::default()
    });
    app.post_api_key_input = "sk-new-key".to_owned();

    let hidden = app.hidden_pending_edit_keys();

    assert_eq!(hidden, vec!["post_api_key".to_owned()]);
}

#[test]
fn hidden_pending_edit_keys_ignores_a_saved_post_api_key() {
    let app = test_app(AppSettings {
        ui_settings_mode: "simple".to_owned(),
        ..AppSettings::default()
    });

    assert!(app.hidden_pending_edit_keys().is_empty());
}

#[test]
fn switching_to_simple_with_a_pending_post_api_key_edit_sets_a_status_hint() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    app.post_api_key_input = "sk-new-key".to_owned();

    app.set_settings_mode(SettingsMode::Simple);

    assert!(
        app.settings_status.contains("post_api_key"),
        "expected a hint naming the hidden post_api_key edit, got: {:?}",
        app.settings_status
    );
}

#[test]
fn switching_to_simple_with_a_hidden_pending_edit_sets_a_status_hint() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    app.settings.device = "cpu".to_owned();

    app.set_settings_mode(SettingsMode::Simple);

    assert!(
        app.settings_status.contains("device"),
        "expected a hint naming the hidden field, got: {:?}",
        app.settings_status
    );
}

#[test]
fn switching_to_simple_without_hidden_pending_edits_leaves_status_untouched() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    app.settings_status = "previous status".to_owned();

    app.set_settings_mode(SettingsMode::Simple);

    assert_eq!(app.settings_status, "previous status");
}

/// Codex P2: `set_settings_mode` must persist ONLY `ui_settings_mode`
/// (via the same single-key `config::set_value` path `wd config set` uses),
/// never resave the whole cached `saved_settings` snapshot — otherwise a
/// concurrent external edit (another `wd config set`, or a hand-edited
/// config.json) to any other key would be silently reverted.
#[test]
fn set_settings_mode_preserves_an_external_edit_to_another_key() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    app.save_settings();

    // An "external" edit landing on disk after this session's own
    // `saved_settings` snapshot was captured — e.g. a `wd config set lang da`
    // run from another terminal while the GUI is open.
    config::set_value("lang", "da", &config).unwrap();

    app.set_settings_mode(SettingsMode::Simple);

    let on_disk = config::AppSettings::from_value(
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        on_disk.lang, "da",
        "an external edit to an unrelated key must survive a settings-mode toggle"
    );
    assert_eq!(on_disk.ui_settings_mode, "simple");
}

/// Codex P2: on a failed write, `saved_settings` must NOT advance — otherwise
/// `has_unsaved_settings` would report clean even though the mode was never
/// actually persisted to disk.
#[test]
fn set_settings_mode_leaves_the_saved_snapshot_stale_when_the_write_fails() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // A directory where the config file is expected forces the read/write to
    // fail with an IO error instead of succeeding.
    let config_as_dir = dir.path().join("config.json");
    std::fs::create_dir(&config_as_dir).unwrap();
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config_as_dir.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    assert!(!app.has_unsaved_settings());

    app.set_settings_mode(SettingsMode::Simple);

    // The choice still applies instantly in the UI...
    assert_eq!(app.settings.ui_settings_mode, "simple");
    // ...but the on-disk snapshot was never advanced, so the app correctly
    // keeps reporting an unsaved change rather than looking clean.
    assert_eq!(app.saved_settings.ui_settings_mode, "advanced");
    assert!(app.has_unsaved_settings());
    assert!(
        app.settings_status.contains("Could not save"),
        "expected a save-failure hint, got: {:?}",
        app.settings_status
    );
}
