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
    let result = try_install(tx, None, std::collections::BTreeMap::new());
    assert!(
        matches!(result, Err(InProcessInstallError::FeaturesMissing)),
        "stock build must refuse in-process install with FeaturesMissing",
    );
    let err = result
        .err()
        .expect("stock build must refuse in-process install");
    let msg = err.to_string();
    assert!(
        msg.contains("rust-hotkeys") && msg.contains("rust-injection"),
        "error must name the missing features: {msg}"
    );
    assert!(
        msg.contains("cargo build --features"),
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
        InProcessInstallError::MissingBackend("audio-in-rust feature not compiled in".to_owned())
            .to_string();
    assert!(
        msg.contains("audio-in-rust feature not compiled in"),
        "must surface the underlying reason: {msg}"
    );
    assert!(
        msg.contains("whisper-rs-local") && msg.contains("rust-injection"),
        "must name the required native rebuild features: {msg}"
    );
    assert!(!msg.contains("fallback"), "{msg}");
}

#[test]
fn apply_worker_command_env_sets_voicepi_keys() {
    // F1 (PR #519 supervisor.rs:467): apply the WorkerCommand's
    // env vector to the process env so `load_settings()` and the real
    // backend constructors see the same resolved native runtime view.
    // Uses the crate-wide ENV_LOCK because it mutates process env.
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());

    let sentinel_lang = "__vp_apply_env_test_lang__";
    let sentinel_prompt = "__vp_apply_env_test_prompt__";
    let previous_lang = std::env::var("VOICEPI_LANG").ok();
    let previous_prompt = std::env::var("VOICEPI_INITIAL_PROMPT").ok();
    let previous_unrelated = std::env::var("UNRELATED_RUNTIME_TEST_KEY").ok();

    std::env::remove_var("VOICEPI_LANG");
    std::env::remove_var("VOICEPI_INITIAL_PROMPT");

    // Sentinel non-VOICEPI value that MUST NOT be applied.
    let unrelated_marker = "__vp_apply_env_test_unrelated__";
    std::env::remove_var("UNRELATED_RUNTIME_TEST_KEY");

    let command = super::worker_command::WorkerCommand {
        program: std::path::PathBuf::from("whisper-dictate"),
        args: Vec::new(),
        working_dir: std::path::PathBuf::from("."),
        env: vec![
            (
                "UNRELATED_RUNTIME_TEST_KEY".to_owned(),
                unrelated_marker.to_owned(),
            ),
            ("VOICEPI_LANG".to_owned(), sentinel_lang.to_owned()),
            (
                "VOICEPI_INITIAL_PROMPT".to_owned(),
                sentinel_prompt.to_owned(),
            ),
        ],
    };

    apply_worker_command_env(&command);

    assert_eq!(
        std::env::var("VOICEPI_LANG").ok().as_deref(),
        Some(sentinel_lang),
        "VOICEPI_LANG must be applied to the process env"
    );
    assert_eq!(
        std::env::var("VOICEPI_INITIAL_PROMPT").ok().as_deref(),
        Some(sentinel_prompt),
        "VOICEPI_INITIAL_PROMPT must be applied to the process env"
    );
    assert!(
        std::env::var("UNRELATED_RUNTIME_TEST_KEY").ok().as_deref() != Some(unrelated_marker),
        "non-VOICEPI values must not be applied"
    );
    restore_session_scoped_env();

    // Restore every env var this test touched.
    match previous_lang {
        Some(v) => std::env::set_var("VOICEPI_LANG", v),
        None => std::env::remove_var("VOICEPI_LANG"),
    }
    match previous_prompt {
        Some(v) => std::env::set_var("VOICEPI_INITIAL_PROMPT", v),
        None => std::env::remove_var("VOICEPI_INITIAL_PROMPT"),
    }
    match previous_unrelated {
        Some(v) => std::env::set_var("UNRELATED_RUNTIME_TEST_KEY", v),
        None => std::env::remove_var("UNRELATED_RUNTIME_TEST_KEY"),
    }
}

#[test]
fn apply_worker_command_env_clobbers_existing_process_env() {
    // The in-process runtime must override process env so a config-file value
    // wins over a stale shell export -- otherwise a user with
    // `lang=da` in the config but a leftover `VOICEPI_LANG=en` in
    // their shell would see the wrong hint.
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let previous = std::env::var("VOICEPI_LANG").ok();

    std::env::set_var("VOICEPI_LANG", "stale-shell-export");

    let command = super::worker_command::WorkerCommand {
        program: std::path::PathBuf::from("whisper-dictate"),
        args: Vec::new(),
        working_dir: std::path::PathBuf::from("."),
        env: vec![("VOICEPI_LANG".to_owned(), "config-value".to_owned())],
    };
    apply_worker_command_env(&command);

    assert_eq!(
        std::env::var("VOICEPI_LANG").ok().as_deref(),
        Some("config-value"),
        "command.env must clobber existing process env"
    );
    restore_session_scoped_env();

    match previous {
        Some(v) => std::env::set_var("VOICEPI_LANG", v),
        None => std::env::remove_var("VOICEPI_LANG"),
    }
}

#[test]
fn worker_log_level_updates_native_debug_and_trace_gates() {
    let _diag_guard = crate::diag_test_lock::DIAG_WRITER_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _env_guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    restore_session_scoped_env();
    let previous = std::env::var_os(crate::diag::LOG_ENV_VAR);
    std::env::remove_var(crate::diag::LOG_ENV_VAR);

    apply_worker_command_env(&super::worker_command::WorkerCommand {
        program: std::path::PathBuf::from("native"),
        args: Vec::new(),
        working_dir: std::path::PathBuf::from("."),
        env: vec![(crate::diag::LOG_ENV_VAR.to_owned(), "trace".to_owned())],
    });

    assert!(crate::diag::debug_enabled());
    assert!(crate::diag::trace_enabled());

    restore_session_scoped_env();
    match previous {
        Some(value) => std::env::set_var(crate::diag::LOG_ENV_VAR, value),
        None => std::env::remove_var(crate::diag::LOG_ENV_VAR),
    }
    crate::diag::init_from_env();
}

#[test]
fn replacement_command_restores_cleared_credential_to_ambient_state() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let previous = std::env::var_os("VOICEPI_STT_API_KEY");
    std::env::set_var("VOICEPI_STT_API_KEY", "ambient-key");

    let with_saved_key = super::worker_command::WorkerCommand {
        program: std::path::PathBuf::from("native-runtime"),
        args: Vec::new(),
        working_dir: std::path::PathBuf::from("."),
        env: vec![("VOICEPI_STT_API_KEY".to_owned(), "saved-key".to_owned())],
    };
    apply_worker_command_env(&with_saved_key);
    assert_eq!(
        std::env::var("VOICEPI_STT_API_KEY").as_deref(),
        Ok("saved-key")
    );

    let after_clear = super::worker_command::WorkerCommand {
        env: Vec::new(),
        ..with_saved_key
    };
    apply_worker_command_env(&after_clear);
    assert_eq!(
        std::env::var("VOICEPI_STT_API_KEY").as_deref(),
        Ok("ambient-key"),
        "an absent key in the replacement command must not reuse the prior saved secret"
    );

    match previous {
        Some(value) => std::env::set_var("VOICEPI_STT_API_KEY", value),
        None => std::env::remove_var("VOICEPI_STT_API_KEY"),
    }
}

#[test]
fn replacement_command_restores_cleared_schema_setting_to_ambient_state() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let previous = std::env::var_os("VOICEPI_AUDIO_DEVICE");
    std::env::set_var("VOICEPI_AUDIO_DEVICE", "ambient-microphone");

    let with_saved_device = super::worker_command::WorkerCommand {
        program: std::path::PathBuf::from("native-runtime"),
        args: Vec::new(),
        working_dir: std::path::PathBuf::from("."),
        env: vec![(
            "VOICEPI_AUDIO_DEVICE".to_owned(),
            "saved-microphone".to_owned(),
        )],
    };
    apply_worker_command_env(&with_saved_device);
    assert_eq!(
        std::env::var("VOICEPI_AUDIO_DEVICE").as_deref(),
        Ok("saved-microphone")
    );

    apply_worker_command_env(&super::worker_command::WorkerCommand {
        env: Vec::new(),
        ..with_saved_device
    });
    assert_eq!(
        std::env::var("VOICEPI_AUDIO_DEVICE").as_deref(),
        Ok("ambient-microphone"),
        "an absent schema setting must not leak the prior session value into restart resolution"
    );

    match previous {
        Some(value) => std::env::set_var("VOICEPI_AUDIO_DEVICE", value),
        None => std::env::remove_var("VOICEPI_AUDIO_DEVICE"),
    }
}
