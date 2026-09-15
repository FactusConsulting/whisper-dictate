//! Unit tests for the pure Simple/Advanced settings-visibility rules in
//! `settings_mode.rs`. No egui context is needed: `tab_visible`,
//! `setting_visible`, and `fallback_tab_for_mode` are plain functions over
//! `Tab`/`SettingsMode`/`&str`.

use super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::*;

const SIMPLE_VISIBLE_TABS: &[Tab] = &[Tab::Log, Tab::Speech, Tab::Output, Tab::System];
const SIMPLE_HIDDEN_TABS: &[Tab] = &[Tab::Quality, Tab::Dictionary, Tab::Post, Tab::Profiles];

#[test]
fn advanced_mode_shows_every_tab() {
    for tab in Tab::ALL {
        assert!(
            tab_visible(SettingsMode::Advanced, tab),
            "{tab:?} must be visible in Advanced mode"
        );
    }
}

#[test]
fn simple_mode_shows_only_the_curated_tabs() {
    for tab in SIMPLE_VISIBLE_TABS {
        assert!(
            tab_visible(SettingsMode::Simple, *tab),
            "{tab:?} must be visible in Simple mode"
        );
    }
    for tab in SIMPLE_HIDDEN_TABS {
        assert!(
            !tab_visible(SettingsMode::Simple, *tab),
            "{tab:?} must be hidden in Simple mode"
        );
    }
}

/// Every Tab::ALL entry falls into exactly one of the visible/hidden lists
/// above — guards against a new Tab variant silently landing nowhere.
#[test]
fn every_tab_is_accounted_for_in_simple_mode() {
    assert_eq!(
        Tab::ALL.len(),
        SIMPLE_VISIBLE_TABS.len() + SIMPLE_HIDDEN_TABS.len()
    );
    for tab in Tab::ALL {
        let in_visible = SIMPLE_VISIBLE_TABS.contains(&tab);
        let in_hidden = SIMPLE_HIDDEN_TABS.contains(&tab);
        assert!(
            in_visible ^ in_hidden,
            "{tab:?} must appear in exactly one of the visible/hidden lists"
        );
    }
}

#[test]
fn advanced_mode_shows_every_schema_key_and_arbitrary_keys() {
    for setting in config::runtime_settings() {
        assert!(
            setting_visible(SettingsMode::Advanced, &setting.key),
            "{} must be visible in Advanced mode",
            setting.key
        );
    }
    assert!(setting_visible(SettingsMode::Advanced, "not_a_real_key"));
}

#[test]
fn simple_mode_shows_every_ui_simple_schema_key() {
    let mut checked = 0;
    for setting in config::runtime_settings() {
        if setting.ui_simple {
            assert!(
                setting_visible(SettingsMode::Simple, &setting.key),
                "ui_simple schema key '{}' must be visible in Simple mode",
                setting.key
            );
            checked += 1;
        }
    }
    // Sanity: the schema actually has ui_simple keys to check (regression
    // guard against this test silently checking nothing).
    assert!(checked >= 5, "expected several ui_simple schema keys");
}

#[test]
fn simple_mode_hides_every_non_ui_simple_schema_key() {
    let mut checked = 0;
    for setting in config::runtime_settings() {
        if !setting.ui_simple {
            assert!(
                !setting_visible(SettingsMode::Simple, &setting.key),
                "non-ui_simple schema key '{}' must be hidden in Simple mode",
                setting.key
            );
            checked += 1;
        }
    }
    assert!(checked >= 5, "expected several non-ui_simple schema keys");
}

/// `ui_simple` is deliberately decoupled from `advanced`: `device` is
/// `advanced: false` (still a "basic" wizard prompt) yet hidden from Simple
/// mode, while `stt_model` is `advanced: true` (still an "advanced" wizard
/// prompt) yet shown in Simple mode. Flipping the desktop UI's Simple/Advanced
/// visibility must never reorder or reshape the wizard's scripted prompts.
#[test]
fn device_and_stt_model_prove_ui_simple_is_independent_of_advanced() {
    let device = config::runtime_settings()
        .iter()
        .find(|setting| setting.key == "device")
        .expect("device is a schema setting");
    assert!(!device.advanced, "device must stay a basic wizard prompt");
    assert!(!setting_visible(SettingsMode::Simple, "device"));

    let stt_model = config::runtime_settings()
        .iter()
        .find(|setting| setting.key == "stt_model")
        .expect("stt_model is a schema setting");
    assert!(
        stt_model.advanced,
        "stt_model must stay an advanced wizard prompt"
    );
    assert!(setting_visible(SettingsMode::Simple, "stt_model"));
}

#[test]
fn simple_mode_shows_the_allow_listed_non_schema_settings() {
    for key in ["stt_provider", "stt_api_key", "ui_language", "ui_theme"] {
        assert!(
            setting_visible(SettingsMode::Simple, key),
            "allow-listed key '{key}' must be visible in Simple mode"
        );
    }
}

#[test]
fn simple_mode_hides_an_unknown_non_schema_key() {
    // A key that is neither in the schema nor the allow-list is treated as
    // advanced-only: a typo hides a field instead of silently exposing it.
    assert!(!setting_visible(
        SettingsMode::Simple,
        "definitely_not_a_setting"
    ));
}

#[test]
fn every_speech_essential_key_from_the_spec_is_visible_in_simple() {
    // The exact Speech-tab fields the feature spec calls "essential":
    // engine, cloud provider, model (local + cloud), API key section,
    // language, push-to-talk key, and microphone.
    for key in [
        "stt_backend",
        "stt_provider",
        "model",
        "stt_model",
        "stt_api_key",
        "lang",
        "key",
        "audio_device",
    ] {
        assert!(
            setting_visible(SettingsMode::Simple, key),
            "essential Speech key '{key}' must be visible in Simple mode"
        );
    }
}

#[test]
fn output_tab_shows_only_inject_mode_in_simple() {
    assert!(setting_visible(SettingsMode::Simple, "inject_mode"));
    for key in [
        "format_commands",
        "command_hook",
        "command_hook_timeout_ms",
        "history_enabled",
        "history_jsonl",
    ] {
        assert!(
            !setting_visible(SettingsMode::Simple, key),
            "Output key '{key}' must be hidden in Simple mode"
        );
    }
}

#[test]
fn fallback_selects_speech_when_current_tab_becomes_hidden() {
    assert_eq!(
        fallback_tab_for_mode(SettingsMode::Simple, Tab::Quality),
        Tab::Speech
    );
    assert_eq!(
        fallback_tab_for_mode(SettingsMode::Simple, Tab::Dictionary),
        Tab::Speech
    );
    assert_eq!(
        fallback_tab_for_mode(SettingsMode::Simple, Tab::Post),
        Tab::Speech
    );
    assert_eq!(
        fallback_tab_for_mode(SettingsMode::Simple, Tab::Profiles),
        Tab::Speech
    );
}

#[test]
fn fallback_keeps_the_current_tab_when_it_stays_visible() {
    for tab in SIMPLE_VISIBLE_TABS {
        assert_eq!(fallback_tab_for_mode(SettingsMode::Simple, *tab), *tab);
    }
}

#[test]
fn fallback_never_changes_the_tab_in_advanced_mode() {
    for tab in Tab::ALL {
        assert_eq!(fallback_tab_for_mode(SettingsMode::Advanced, tab), tab);
    }
}

#[test]
fn mode_id_roundtrips_and_unknown_raw_defaults_to_advanced() {
    assert_eq!(SettingsMode::from_raw("simple"), SettingsMode::Simple);
    assert_eq!(SettingsMode::from_raw("advanced"), SettingsMode::Advanced);
    // No key in config.json ("") and any unrecognized value both fall back to
    // Advanced, matching AppSettings::default() — an existing user's config
    // sees no behaviour change.
    assert_eq!(SettingsMode::from_raw(""), SettingsMode::Advanced);
    assert_eq!(SettingsMode::from_raw("bogus"), SettingsMode::Advanced);
    assert_eq!(SettingsMode::Simple.id(), "simple");
    assert_eq!(SettingsMode::Advanced.id(), "advanced");
}

#[test]
fn mode_labels_are_localized_and_distinct_en_and_da() {
    for lang in ["en", "da"] {
        let simple = SettingsMode::Simple.label(lang);
        let advanced = SettingsMode::Advanced.label(lang);
        assert!(!simple.is_empty());
        assert!(!advanced.is_empty());
        assert_ne!(simple, advanced);
    }
    assert_ne!(
        SettingsMode::Simple.label("en"),
        SettingsMode::Simple.label("da")
    );
    assert_ne!(
        SettingsMode::Advanced.label("en"),
        SettingsMode::Advanced.label("da")
    );
}

#[test]
fn default_app_settings_use_advanced_mode() {
    assert_eq!(AppSettings::default().ui_settings_mode, "advanced");
}

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
