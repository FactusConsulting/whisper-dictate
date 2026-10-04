// Mode-shortcut worker tests: pure rotation logic plus the config
// read-modify-write the worker performs per press.

use super::{resolve_post_mode, run, ModeRequest, RuntimeEvent, POST_MODE_CHOICES};
use crate::config::test_support::{restore_env, CONFIG_ENV, ENV_LOCK};

#[test]
fn cycle_wraps_through_the_schema_order() {
    // Walking the whole cycle must return to the starting mode.
    let mut mode = String::from("raw");
    for expected in POST_MODE_CHOICES
        .iter()
        .skip(1)
        .chain(POST_MODE_CHOICES.first())
    {
        mode = super::next_post_mode(&mode);
        assert_eq!(mode, *expected, "cycling from the previous mode");
    }
    // A wrap after the last choice lands back on raw.
    assert_eq!(super::next_post_mode("bullets"), "raw");
}

#[test]
fn unknown_current_mode_snaps_forward_to_raw() {
    // A hand-edited config value is not part of the cycle; the first
    // cycle press should land on a known choice instead of carrying the
    // garbage forward.
    assert_eq!(super::next_post_mode("wibble"), "raw");
    assert_eq!(super::next_post_mode(""), "raw");
}

#[test]
fn raw_and_clean_requests_pin_their_mode() {
    assert_eq!(resolve_post_mode(ModeRequest::Raw, "bullets"), "raw");
    assert_eq!(resolve_post_mode(ModeRequest::Clean, "raw"), "clean");
    // Raw/clean ignore the current value entirely.
    assert_eq!(resolve_post_mode(ModeRequest::Raw, "clean"), "raw");
    assert_eq!(resolve_post_mode(ModeRequest::Clean, "prompt"), "clean");
}

#[test]
fn cycle_request_persists_and_reports_the_new_mode() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.json");
    std::fs::write(&config_path, r#"{"post_mode": "clean"}"#).unwrap();
    let previous = std::env::var_os(CONFIG_ENV);
    std::env::set_var(CONFIG_ENV, &config_path);
    let event = run(ModeRequest::Cycle);
    restore_env(CONFIG_ENV, previous);
    assert!(
        matches!(event, RuntimeEvent::Stdout(ref line) if line == "[hotkey] post mode: prompt"),
        "{event:?}"
    );
    // The change persisted to the config the press read from.
    let reloaded = crate::config::load_settings_from_path(&config_path).unwrap();
    assert_eq!(reloaded.post_mode, "prompt");
}

#[test]
fn raw_request_persists_raw_over_a_profile_value() {
    // The global post_mode is what the shortcut owns; per-window profile
    // overrides continue to win at dictation time, but the persisted
    // global value must reflect the press.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.json");
    std::fs::write(&config_path, r#"{"post_mode": "bullets"}"#).unwrap();
    let previous = std::env::var_os(CONFIG_ENV);
    std::env::set_var(CONFIG_ENV, &config_path);
    let event = run(ModeRequest::Raw);
    restore_env(CONFIG_ENV, previous);
    assert!(
        matches!(event, RuntimeEvent::Stdout(ref line) if line == "[hotkey] post mode: raw"),
        "{event:?}"
    );
    let reloaded = crate::config::load_settings_from_path(&config_path).unwrap();
    assert_eq!(reloaded.post_mode, "raw");
}

#[test]
fn clean_request_persists_clean() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.json");
    std::fs::write(&config_path, r#"{"post_mode": "raw"}"#).unwrap();
    let previous = std::env::var_os(CONFIG_ENV);
    std::env::set_var(CONFIG_ENV, &config_path);
    let event = run(ModeRequest::Clean);
    restore_env(CONFIG_ENV, previous);
    assert!(
        matches!(event, RuntimeEvent::Stdout(ref line) if line == "[hotkey] post mode: clean"),
        "{event:?}"
    );
    let reloaded = crate::config::load_settings_from_path(&config_path).unwrap();
    assert_eq!(reloaded.post_mode, "clean");
}
