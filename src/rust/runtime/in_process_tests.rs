//! Unit tests for [`super::in_process`]. Extracted from the sibling
//! round to keep the production module under the AGENTS.md 500-LOC
//! modularity limit (PR #519 in_process.rs:444).

use super::in_process::*;
use super::supervisor::RuntimeEvent;
use std::sync::mpsc;

// ---------------------------------------------------------------------
// Push-to-talk ownership refusal stays distinct so diagnostics identify
// a second process holding the chord.
// ---------------------------------------------------------------------

#[test]
fn an_ownership_refusal_classifies_as_its_own_variant() {
    let classified = classify_hotkey_install_error(crate::hotkey::InstallError::AlreadyHeld {
        chord: "f9".to_owned(),
        holder_pid: Some(12345),
        holder_desc: "pid 12345 (whisper-dictate-gui)".to_owned(),
    });
    assert!(
        matches!(classified, InProcessInstallError::PttAlreadyHeld(_)),
        "the ownership refusal must stay distinguishable from a generic \
         install failure; got {classified:?}"
    );
    // The whole refusal text has to survive: it is what the operator
    // reads, and it names the pid to quit.
    let rendered = classified.to_string();
    assert!(rendered.contains("pid 12345"), "{rendered}");
    assert!(rendered.contains("interleaving"), "{rendered}");
    assert!(rendered.contains("will not respond"), "{rendered}");
    assert!(rendered.is_ascii(), "console output must be ASCII");
}

#[test]
fn every_other_install_error_is_actionable_without_fallback() {
    use crate::hotkey::InstallError;
    assert!(matches!(
        classify_hotkey_install_error(InstallError::Unsupported),
        InProcessInstallError::FeaturesMissing
    ));
    assert!(matches!(
        classify_hotkey_install_error(InstallError::EmptyConfig),
        InProcessInstallError::HotkeyInstallFailed(_)
    ));
    assert!(matches!(
        classify_hotkey_install_error(InstallError::UnsupportedKey("super_l".to_owned())),
        InProcessInstallError::HotkeyInstallFailed(_)
    ));
    let listener =
        classify_hotkey_install_error(InstallError::ListenerStartup("no X display".to_owned()));
    assert!(matches!(
        listener,
        InProcessInstallError::HotkeyInstallFailed(_)
    ));
    assert!(!listener.to_string().contains("fallback"));
}

#[test]
fn features_available_matches_cfg() {
    assert_eq!(
        features_available(),
        cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
    );
}

#[test]
fn ready_worker_event_shape_matches_python_ready() {
    // Contract with the UI: emit a WorkerEvent whose `event="status"`
    // and `state=Some("ready")` so `worker_ready_for_state("ready")`
    // fires the same latch the Python worker triggers. Regression
    // test for design doc risk #2.
    let (tx, rx) = mpsc::channel();
    emit_ready_worker_event(&tx);
    let received = rx.try_recv().expect("ready worker event enqueued");
    match received {
        RuntimeEvent::Worker(worker) => {
            assert_eq!(worker.event, "status");
            assert_eq!(worker.state.as_deref(), Some("ready"));
            // The `engine` field is Phase B-specific so operators
            // can tell an in-process ready apart from a Python one.
            assert_eq!(
                worker.payload.get("engine").and_then(|v| v.as_str()),
                Some("rust"),
            );
        }
        other => panic!("expected RuntimeEvent::Worker, got {other:?}"),
    }
}

#[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
#[test]
fn try_install_stock_build_returns_features_missing() {
    // On a stock build the supervisor's Phase B branch MUST fail
    // fast with an actionable message so the caller can fall back
    // to the Python worker without spinning up any threads. This
    // pins the contract the fallback path relies on.
    let (tx, _rx) = mpsc::channel();
    let result = try_install(
        tx,
        None,
        super::settings_snapshot::RuntimeSettingsSnapshot::default(),
        std::collections::BTreeMap::new(),
    );
    assert!(
        matches!(result, Err(InProcessInstallError::FeaturesMissing)),
        "stock build must refuse in-process install with FeaturesMissing",
    );
    let err = result
        .err()
        .expect("stock build must refuse in-process install");
    let msg = err.to_string();
    assert!(
        msg.contains("canonical native backends") && msg.contains("shipping"),
        "error must name the canonical shipping profile: {msg}"
    );
    assert!(
        msg.contains("cargo build --no-default-features --features shipping"),
        "error must include the rebuild command: {msg}"
    );
}

#[test]
fn catch_unwind_panic_string_literal_lands_as_panicked_error() {
    // Design doc risk #3: a panic inside the install path must
    // convert into a recoverable InProcessInstallError::Panicked
    // rather than aborting the UI process. This pins the
    // stringifier that runs on the recovery path so a future
    // refactor that swaps `catch_unwind` for something else is
    // caught by a test failure. Feature-independent because the
    // stringifier itself is pure.
    let payload = std::panic::catch_unwind(|| panic!("boom-from-test"))
        .expect_err("literal panic must land in catch_unwind Err arm");
    let msg = stringify_panic(payload);
    assert!(
        msg.contains("boom-from-test"),
        "stringifier lost the payload: {msg}"
    );
    // And the same round-trips for owned-String payloads (which is
    // what `assert!(false, "…")` produces internally).
    let payload = std::panic::catch_unwind(|| panic!("owned {}", "message"))
        .expect_err("formatted panic must land in Err");
    let msg = stringify_panic(payload);
    assert!(
        msg.contains("owned message"),
        "stringifier lost owned payload: {msg}"
    );
}

#[test]
fn env_precedence_note_fires_only_when_both_env_vars_set() {
    // Design doc risk #5: with BOTH `VOICEPI_DICTATE_ENGINE=rust`
    // AND `VOICEPI_DICTATE_BACKEND=rust-session` set, the
    // supervisor emits an informational line naming the effective
    // backend. With only ENGINE=rust set, no line fires.
    //
    // Uses the crate-wide ENV_LOCK so this test serialises with the
    // other Rust unit tests that mutate `VOICEPI_DICTATE_BACKEND`
    // (PR #519 in_process.rs:594).
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let previous = std::env::var("VOICEPI_DICTATE_BACKEND").ok();

    // Case 1: backend unset - no line.
    std::env::remove_var("VOICEPI_DICTATE_BACKEND");
    let (tx, rx) = mpsc::channel();
    maybe_emit_env_precedence_note(&tx);
    assert!(rx.try_recv().is_err(), "no line without rust-session set");

    // Case 2: backend set to rust-session - informational line
    // fires naming both env vars.
    std::env::set_var("VOICEPI_DICTATE_BACKEND", "rust-session");
    let (tx, rx) = mpsc::channel();
    maybe_emit_env_precedence_note(&tx);
    match rx.try_recv().expect("precedence note enqueued") {
        RuntimeEvent::Stderr(line) => {
            assert!(
                line.contains("VOICEPI_DICTATE_ENGINE"),
                "line names ENGINE: {line}"
            );
            assert!(
                line.contains("VOICEPI_DICTATE_BACKEND"),
                "line names BACKEND: {line}"
            );
            assert!(line.contains("wins"), "line names the precedence: {line}");
        }
        other => panic!("expected Stderr, got {other:?}"),
    }

    // Restore.
    match previous {
        Some(v) => std::env::set_var("VOICEPI_DICTATE_BACKEND", v),
        None => std::env::remove_var("VOICEPI_DICTATE_BACKEND"),
    }
}

#[test]
fn install_error_display_covers_every_variant() {
    // Sonar-friendly: every user-facing error variant must have a
    // non-empty Display impl so the supervisor's stderr forwarding
    // has something to log. Missing a variant here is a refactor
    // regression signal.
    assert!(!InProcessInstallError::FeaturesMissing
        .to_string()
        .is_empty());
    assert!(!InProcessInstallError::ConfigLoadFailed("boom".to_owned())
        .to_string()
        .is_empty());
    assert!(!InProcessInstallError::EmptyChord.to_string().is_empty());
    assert!(!InProcessInstallError::MissingBackend("nope".to_owned())
        .to_string()
        .is_empty());
    assert!(
        !InProcessInstallError::HotkeyInstallFailed("nope".to_owned())
            .to_string()
            .is_empty()
    );
    assert!(!InProcessInstallError::Panicked("crash".to_owned())
        .to_string()
        .is_empty());
    assert!(!InProcessInstallError::PttAlreadyHeld("refused".to_owned())
        .to_string()
        .is_empty());
}

#[test]
fn missing_backend_display_names_reason_and_rebuild_features() {
    let msg =
        InProcessInstallError::MissingBackend("audio-capture feature not compiled in".to_owned())
            .to_string();
    assert!(
        msg.contains("audio-capture feature not compiled in"),
        "must surface the underlying reason: {msg}"
    );
    assert!(
        msg.contains("--no-default-features --features shipping"),
        "must name the canonical native rebuild profile: {msg}"
    );
    assert!(!msg.contains("fallback"), "{msg}");
}

#[test]
fn runtime_snapshots_never_mutate_process_settings_or_credentials() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let previous_lang = std::env::var_os("VOICEPI_LANG");
    let previous_key = std::env::var_os("VOICEPI_STT_API_KEY");
    std::env::set_var("VOICEPI_LANG", "ambient-language");
    std::env::remove_var("VOICEPI_STT_API_KEY");

    let first = super::settings_snapshot::RuntimeSettingsSnapshot::from_pairs(vec![
        ("VOICEPI_LANG".to_owned(), "session-language".to_owned()),
        (
            "VOICEPI_STT_API_KEY".to_owned(),
            "session-secret".to_owned(),
        ),
    ])
    .unwrap();
    let replacement = super::settings_snapshot::RuntimeSettingsSnapshot::default();

    assert_eq!(first.value("VOICEPI_LANG"), Some("session-language"));
    assert_eq!(first.value("VOICEPI_STT_API_KEY"), Some("session-secret"));
    assert_eq!(replacement.value("VOICEPI_STT_API_KEY"), None);
    assert_eq!(
        std::env::var("VOICEPI_LANG").as_deref(),
        Ok("ambient-language")
    );
    assert!(std::env::var("VOICEPI_STT_API_KEY").is_err());

    match previous_lang {
        Some(value) => std::env::set_var("VOICEPI_LANG", value),
        None => std::env::remove_var("VOICEPI_LANG"),
    }
    match previous_key {
        Some(value) => std::env::set_var("VOICEPI_STT_API_KEY", value),
        None => std::env::remove_var("VOICEPI_STT_API_KEY"),
    }
}
