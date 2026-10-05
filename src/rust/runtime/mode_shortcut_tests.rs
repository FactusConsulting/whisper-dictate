// Mode-shortcut worker tests: pure rotation logic plus the config
// read-modify-write the worker performs per press.

use super::{resolve_post_mode, run, ModeRequest, RuntimeEvent, POST_MODE_CHOICES};
use crate::config::test_support::{restore_env, CONFIG_ENV, ENV_LOCK};
use crate::postprocess::POST_MODE_ENV;

fn logged_mode_line(events: &[RuntimeEvent], expected: &str) -> bool {
    events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Stdout(line) if line == expected))
}

fn reported_mode(events: &[RuntimeEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        RuntimeEvent::Worker(worker) if worker.event == "post_mode_changed" => worker
            .payload
            .get("mode")
            .and_then(|mode| mode.as_str())
            .map(str::to_owned),
        _ => None,
    })
}

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
    let events = run(ModeRequest::Cycle);
    restore_env(CONFIG_ENV, previous);
    assert!(
        logged_mode_line(&events, "[hotkey] post mode: prompt"),
        "{events:?}"
    );
    // The structured event carries the new mode so the Settings page can
    // reconcile its snapshots without discarding pending edits.
    assert_eq!(reported_mode(&events).as_deref(), Some("prompt"));
    // The change persisted to the config the press read from.
    let reloaded = crate::config::load_settings_from_path(&config_path).unwrap();
    assert_eq!(reloaded.post_mode, "prompt");
}

#[test]
fn mode_press_writes_only_the_post_mode_key() {
    // a whole-snapshot save would
    // materialize defaults into a sparse config.json and override the
    // environment fallbacks the user relies on. The worker must write
    // ONLY the post_mode key and leave every other key untouched.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.json");
    std::fs::write(
        &config_path,
        r#"{"post_mode": "raw", "history_enabled": false, "device": "cuda"}"#,
    )
    .unwrap();
    let previous = std::env::var_os(CONFIG_ENV);
    std::env::set_var(CONFIG_ENV, &config_path);
    let events = run(ModeRequest::Clean);
    restore_env(CONFIG_ENV, previous);
    assert!(
        logged_mode_line(&events, "[hotkey] post mode: clean"),
        "{events:?}"
    );
    let reloaded = crate::config::load_settings_from_path(&config_path).unwrap();
    assert_eq!(reloaded.post_mode, "clean");
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    // Sparse unrelated keys survive byte-for-byte in meaning.
    assert_eq!(raw["history_enabled"], serde_json::Value::Bool(false));
    assert_eq!(raw["device"], "cuda");
    // And the save did not materialize a full typed snapshot.
    assert!(
        raw.as_object().unwrap().len() <= 3,
        "the save wrote unrelated keys: {raw}"
    );
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
    let events = run(ModeRequest::Raw);
    restore_env(CONFIG_ENV, previous);
    assert!(
        logged_mode_line(&events, "[hotkey] post mode: raw"),
        "{events:?}"
    );
    assert_eq!(reported_mode(&events).as_deref(), Some("raw"));
    let reloaded = crate::config::load_settings_from_path(&config_path).unwrap();
    assert_eq!(reloaded.post_mode, "raw");
}

#[test]
fn cycle_starts_from_the_environment_provided_mode() {
    // The live runtime resolves post_mode from the config file, then the
    // process environment, then the schema default. When config.json has
    // no post_mode but VOICEPI_POST_MODE is set, the first press must
    // advance the mode the runtime is actually using (email -> bullets),
    // not the schema default.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.json");
    std::fs::write(&config_path, r#"{"history_enabled": false}"#).unwrap();
    let previous_config = std::env::var_os(CONFIG_ENV);
    let previous_mode = std::env::var_os(POST_MODE_ENV);
    std::env::set_var(CONFIG_ENV, &config_path);
    std::env::set_var(POST_MODE_ENV, "email");
    let events = run(ModeRequest::Cycle);
    restore_env(CONFIG_ENV, previous_config);
    restore_env(POST_MODE_ENV, previous_mode);
    assert!(
        logged_mode_line(&events, "[hotkey] post mode: bullets"),
        "{events:?}"
    );
    assert_eq!(reported_mode(&events).as_deref(), Some("bullets"));
    let reloaded = crate::config::load_settings_from_path(&config_path).unwrap();
    assert_eq!(reloaded.post_mode, "bullets");
}

#[test]
fn config_mode_wins_over_the_environment_mode() {
    // A config.json post_mode outranks VOICEPI_POST_MODE in the runtime
    // precedence, so the cycle starts from the config value.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.json");
    std::fs::write(&config_path, r#"{"post_mode": "clean"}"#).unwrap();
    let previous_config = std::env::var_os(CONFIG_ENV);
    let previous_mode = std::env::var_os(POST_MODE_ENV);
    std::env::set_var(CONFIG_ENV, &config_path);
    std::env::set_var(POST_MODE_ENV, "email");
    let events = run(ModeRequest::Cycle);
    restore_env(CONFIG_ENV, previous_config);
    restore_env(POST_MODE_ENV, previous_mode);
    assert!(
        logged_mode_line(&events, "[hotkey] post mode: prompt"),
        "{events:?}"
    );
}

#[test]
fn cycle_normalizes_non_canonical_environment_modes() {
    // The live postprocessor normalizes aliases and case before every
    // dictation; the cycle must do the same so VOICEPI_POST_MODE in a
    // non-canonical form advances from the mode it actually resolves to.
    for (raw_mode, expected_line) in [
        // bullet-list normalizes to bullets, which is the LAST choice, so
        // the cycle wraps to raw.
        ("bullet-list", "raw"),
        ("EMAIL", "bullets"),
        (" raw ", "clean"),
    ] {
        let events = {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let config_dir = tempfile::tempdir().unwrap();
            let config_path = config_dir.path().join("config.json");
            std::fs::write(&config_path, r#"{}"#).unwrap();
            let previous_config = std::env::var_os(CONFIG_ENV);
            let previous_mode = std::env::var_os(POST_MODE_ENV);
            std::env::set_var(CONFIG_ENV, &config_path);
            std::env::set_var(POST_MODE_ENV, raw_mode);
            let events = run(ModeRequest::Cycle);
            restore_env(CONFIG_ENV, previous_config);
            restore_env(POST_MODE_ENV, previous_mode);
            events
        };
        assert!(
            logged_mode_line(&events, &format!("[hotkey] post mode: {expected_line}")),
            "raw mode {raw_mode:?} produced {events:?}"
        );
    }
}

#[test]
fn clean_request_persists_clean() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.json");
    std::fs::write(&config_path, r#"{"post_mode": "raw"}"#).unwrap();
    let previous = std::env::var_os(CONFIG_ENV);
    std::env::set_var(CONFIG_ENV, &config_path);
    let events = run(ModeRequest::Clean);
    restore_env(CONFIG_ENV, previous);
    assert!(
        logged_mode_line(&events, "[hotkey] post mode: clean"),
        "{events:?}"
    );
    assert_eq!(reported_mode(&events).as_deref(), Some("clean"));
    let reloaded = crate::config::load_settings_from_path(&config_path).unwrap();
    assert_eq!(reloaded.post_mode, "clean");
}
