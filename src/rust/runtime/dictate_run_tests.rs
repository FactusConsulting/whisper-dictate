//! Tests for the native terminal dictation driver.

use super::*;

#[test]
fn split_key_names_single_key() {
    assert_eq!(split_key_names("ctrl_r"), vec!["ctrl_r".to_owned()]);
}

#[test]
fn split_key_names_multi_key_chord() {
    assert_eq!(
        split_key_names("ctrl_l+shift_l+l"),
        vec!["ctrl_l".to_owned(), "shift_l".to_owned(), "l".to_owned()]
    );
}

#[test]
fn split_key_names_trims_and_drops_empty() {
    // Mirrors `hotkey::capture::split_key_names` so a config that
    // installs under `hotkey capture` installs identically here.
    assert_eq!(
        split_key_names("  ctrl_l +  + shift_r "),
        vec!["ctrl_l".to_owned(), "shift_r".to_owned()]
    );
}

#[test]
fn split_key_names_empty_input_yields_empty_vec() {
    assert!(split_key_names("").is_empty());
    assert!(split_key_names("   ").is_empty());
    assert!(split_key_names("+ + +").is_empty());
}

#[test]
fn native_runtime_options_fail_before_session_start() {
    let vulkan = validate_native_runtime_options(Some("vulkan"), Some("whisper"), false)
        .expect_err("local Vulkan must fail on a CPU-only build");
    assert!(vulkan.to_string().contains("CPU-only"));

    validate_native_runtime_options(Some("cpu"), Some("whisper"), false).unwrap();
    validate_native_runtime_options(Some("vulkan"), Some("whisper"), true).unwrap();
    validate_native_runtime_options(Some("vulkan"), Some("openai"), false)
        .expect("cloud STT ignores the local Vulkan device hint");
    validate_native_runtime_options(Some("cuda"), Some("whisper"), false)
        .expect_err("legacy CUDA alias must receive the same CPU-only guard");
}

#[test]
fn terminal_capture_exit_is_a_terminal_runtime_event() {
    assert!(terminal_event_ends_runtime(
        &crate::runtime::RuntimeEvent::Exited { code: Some(1) }
    ));
    assert!(!terminal_event_ends_runtime(
        &crate::runtime::RuntimeEvent::Stderr("capture warning".to_owned())
    ));
}

#[test]
fn terminal_driver_closes_injection_gate_before_joining_hotkey_threads() {
    let source = include_str!("dictate_run.rs");
    let gate = source
        .find("runtime_active.store(false")
        .expect("terminal shutdown must close the injection gate");
    let shutdown = source
        .find("handle.shutdown()")
        .expect("terminal shutdown must join hotkey threads");
    assert!(
        gate < shutdown,
        "Ctrl-C/capture failure must suppress late injection before coordinator join"
    );
}
#[test]
fn effective_json_events_honors_cli_config_and_environment() {
    assert!(effective_json_events(true, None));
    assert!(effective_json_events(false, Some("1")));
    assert!(effective_json_events(false, Some(" true ")));
    assert!(!effective_json_events(false, Some("false")));
    assert!(!effective_json_events(false, None));
}

#[test]
fn releasing_root_sender_allows_component_teardown_to_disconnect() {
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    let (root, rx) = mpsc::channel::<()>();
    let component = root.clone();
    release_root_sender(root);
    assert_eq!(
        rx.recv_timeout(Duration::ZERO),
        Err(RecvTimeoutError::Timeout),
        "a live component sender must keep the channel connected"
    );
    drop(component);
    assert_eq!(
        rx.recv_timeout(Duration::ZERO),
        Err(RecvTimeoutError::Disconnected),
        "dropping the final component must wake the foreground loop"
    );
}
#[test]
fn features_available_matches_cfg() {
    // Pin the gate so a refactor of `cfg!` at the call site is caught.
    assert_eq!(
        features_available(),
        cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
    );
}

#[test]
fn production_features_available_matches_cfg() {
    assert_eq!(
        production_features_available(),
        cfg!(all(
            feature = "rust-hotkeys",
            feature = "rust-injection",
            feature = "audio-capture",
            feature = "whisper-rs-local"
        ))
    );
}

#[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
#[test]
fn stock_build_returns_actionable_rebuild_message() {
    // The stock build MUST NOT install anything — it should fail fast
    // with a message that names the missing features and the rebuild
    // command. This is the contract the Python parent (Phase A step 2)
    // will rely on to distinguish "feature not built" from a runtime
    // failure it should surface.
    let err = handle_dictate_run(DictateRunArgs {
        config: None,
        json_events: false,
        foreground: false,
        env_overrides: Vec::new(),
    })
    .expect_err("stock build must refuse dictate-run");
    let msg = err.to_string();
    assert!(
        msg.contains("rust-hotkeys") && msg.contains("rust-injection"),
        "error must name both required features: {msg}"
    );
    assert!(
        msg.contains("cargo build --features"),
        "error must include the rebuild command: {msg}"
    );
}
