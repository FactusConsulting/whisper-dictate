//! Companion tests for `hotkey/preflight.rs` - the Rust-hotkey backend
//! selection gates, chord validation, and preflight inspection.
//!
//! The env gates are pure helpers so they are unit-testable without
//! spawning threads; the preflight behaviour tests need Windows (the
//! RegisterHotKey chord table lives behind `target_os = "windows"`).

#[cfg(not(feature = "rust-hotkeys"))]
use super::validate_key_names;
#[cfg(all(test, feature = "rust-hotkeys"))]
use crate::hotkey::config::InstallError;
#[cfg(all(test, target_os = "windows", feature = "rust-hotkeys"))]
use crate::hotkey::manager::DriverKind;
use crate::hotkey::{rust_hotkey_backend_available, rust_hotkey_backend_requested};

#[test]
fn backend_requested_reads_env_var_truthy_rust_only() {
    // Hold the crate-wide env lock so we don't race other env-mutating
    // tests in the same binary — see crate::test_env_lock for the
    // soundness contract.
    let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let prev = std::env::var("VOICEPI_HOTKEY_BACKEND").ok();

    std::env::remove_var("VOICEPI_HOTKEY_BACKEND");
    assert!(!rust_hotkey_backend_requested());

    std::env::set_var("VOICEPI_HOTKEY_BACKEND", "rust");
    assert!(rust_hotkey_backend_requested());

    std::env::set_var("VOICEPI_HOTKEY_BACKEND", "RUST");
    assert!(rust_hotkey_backend_requested());

    std::env::set_var("VOICEPI_HOTKEY_BACKEND", "pynput");
    assert!(!rust_hotkey_backend_requested());

    std::env::set_var("VOICEPI_HOTKEY_BACKEND", "");
    assert!(!rust_hotkey_backend_requested());

    match prev {
        Some(v) => std::env::set_var("VOICEPI_HOTKEY_BACKEND", v),
        None => std::env::remove_var("VOICEPI_HOTKEY_BACKEND"),
    }
}

/// `rust_hotkey_backend_available` reflects the feature gate, not an env
/// var, so it is constant per binary build. On a stock build it must
/// always be false; on a `rust-hotkeys` build it must always be true.
#[test]
fn backend_available_reflects_feature_gate() {
    // This assertion is always true by the cfg definition:
    assert_eq!(
        rust_hotkey_backend_available(),
        cfg!(feature = "rust-hotkeys"),
        "backend_available must equal the rust-hotkeys feature flag"
    );
}

/// When the user sets `VOICEPI_HOTKEY_BACKEND=rust` but the binary was
/// built without `--features rust-hotkeys`, `backend_available` must be
/// false (the env var controls `requested`, not `available`).
#[test]
#[cfg(not(feature = "rust-hotkeys"))]
fn backend_available_is_false_on_stock_build_regardless_of_env() {
    let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let prev = std::env::var("VOICEPI_HOTKEY_BACKEND").ok();

    std::env::set_var("VOICEPI_HOTKEY_BACKEND", "rust");
    assert!(
        !rust_hotkey_backend_available(),
        "backend_available must be false on a stock build even when env var is set"
    );

    match prev {
        Some(v) => std::env::set_var("VOICEPI_HOTKEY_BACKEND", v),
        None => std::env::remove_var("VOICEPI_HOTKEY_BACKEND"),
    }
}

#[test]
#[cfg(not(feature = "rust-hotkeys"))]
fn validate_key_names_stub_always_returns_ok() {
    // Empty input: the feature build rejects with EmptyConfig; the
    // stub is unconditionally Ok.
    assert!(validate_key_names(&[]).is_ok());
    // Non-empty input including a name the feature build would reject
    // (`super_l` is not in the rdev key map): stub still returns Ok
    // because the supervisor never consults the result when
    // `hotkey_handle` is None.
    assert!(validate_key_names(&["ctrl_l".to_owned(), "super_l".to_owned()]).is_ok());
}

#[cfg(all(test, target_os = "windows", feature = "rust-hotkeys"))]
#[test]
fn windows_preflight_uses_register_for_a_generic_chord_without_env_mutation() {
    let pause =
        super::preflight_hotkey_for_kind(&["pause".to_owned()], DriverKind::Register).unwrap();
    assert_eq!(pause.planned_driver, "win_registerhotkey");
    assert_eq!(pause.fallback_reason, None);
    assert!(!pause.focused_window_risk);

    let result = super::preflight_hotkey_for_kind(
        &["ctrl".to_owned(), "f9".to_owned()],
        DriverKind::Register,
    )
    .unwrap();
    assert_eq!(result.planned_driver, "win_registerhotkey");
    assert_eq!(result.fallback_reason, None);
    assert!(!result.focused_window_risk);
}

#[cfg(all(test, target_os = "windows", feature = "rust-hotkeys"))]
#[test]
fn windows_preflight_reports_real_fallback_and_focus_risk() {
    let result = super::preflight_hotkey_for_kind(
        &["ctrl_l".to_owned(), "f9".to_owned()],
        DriverKind::Register,
    )
    .unwrap();
    assert_eq!(result.planned_driver, "rdev");
    assert!(result
        .fallback_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("side-specific")));
    assert!(result.focused_window_risk);
}

#[cfg(all(test, feature = "rust-hotkeys"))]
#[test]
fn preflight_rejects_a_known_config_token_the_selected_listener_cannot_deliver() {
    let error = super::preflight_hotkey_for_kind(&["insert".to_owned()], DriverKind::Rdev)
        .expect_err("insert is not in the rdev delivery table");
    assert!(matches!(error, InstallError::UnsupportedKey(name) if name == "insert"));
}
