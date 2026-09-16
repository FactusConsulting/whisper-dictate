//! Unit tests for the pure Simple/Advanced settings-visibility rules in
//! `settings_mode.rs`. No egui context is needed: `tab_visible`,
//! `setting_visible`, and `fallback_tab_for_mode` are plain functions over
//! `Tab`/`SettingsMode`/`&str`.
//!
//! Split (Codex P2, this file was 563 lines) into three focused modules that
//! share these visibility-rule tests plus the `test_app` fixture:
//! - `settings_mode_tests.rs` (here) — pure visibility rules.
//! - `settings_mode_persistence_tests.rs` — config-file / dirty-tracking /
//!   hidden-pending-edit behaviour of `set_settings_mode`.
//! - `settings_mode_navigation_tests.rs` — `select_tab` / `reload_settings`
//!   tab-selection fallback.

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
