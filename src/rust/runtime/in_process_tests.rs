//! Unit tests for [`super::in_process`]. Extracted from the sibling
//! `in_process.rs` module in the Phase B step 1 review-response
//! round to keep the production module under the AGENTS.md 500-LOC
//! modularity limit (Codex P2 PR #519 in_process.rs:444).

use super::in_process::*;
use super::supervisor::RuntimeEvent;
use std::sync::mpsc;

#[test]
fn engine_choice_unset_is_python() {
    assert_eq!(EngineChoice::from_env_value(None), EngineChoice::Python);
}

#[test]
fn engine_choice_blank_is_python() {
    assert_eq!(EngineChoice::from_env_value(Some("")), EngineChoice::Python);
    assert_eq!(
        EngineChoice::from_env_value(Some("   ")),
        EngineChoice::Python
    );
}

#[test]
fn engine_choice_explicit_python() {
    assert_eq!(
        EngineChoice::from_env_value(Some("python")),
        EngineChoice::Python
    );
    // Case-insensitive so `PYTHON`, `Python` and stray whitespace
    // all resolve to the same canonical variant.
    assert_eq!(
        EngineChoice::from_env_value(Some(" Python ")),
        EngineChoice::Python
    );
}

#[test]
fn engine_choice_rust() {
    assert_eq!(
        EngineChoice::from_env_value(Some("rust")),
        EngineChoice::Rust
    );
    assert_eq!(
        EngineChoice::from_env_value(Some("RUST")),
        EngineChoice::Rust
    );
    assert_eq!(
        EngineChoice::from_env_value(Some(" rust ")),
        EngineChoice::Rust
    );
}

#[test]
fn engine_choice_unknown_carries_raw_value() {
    match EngineChoice::from_env_value(Some("go")) {
        EngineChoice::Unknown(raw) => assert_eq!(raw, "go"),
        other => panic!("expected Unknown(\"go\"), got {other:?}"),
    }
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
    let result = try_install(tx, None);
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
    // (Codex P2 PR #519 in_process.rs:594).
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
}

#[test]
fn missing_backend_display_names_reason_and_fallback() {
    // The MissingBackend variant is what triggers the Python
    // fallback when a stock-ish build cannot construct the real
    // whisper + inject session. Display must include the raw
    // reason (so users can act on it) and name the fallback
    // (so users understand why the Python worker took over).
    let msg =
        InProcessInstallError::MissingBackend("audio-in-rust feature not compiled in".to_owned())
            .to_string();
    assert!(
        msg.contains("audio-in-rust feature not compiled in"),
        "must surface the underlying reason: {msg}"
    );
    assert!(
        msg.contains("Python"),
        "must name the fallback path so operators know what took over: {msg}"
    );
}

#[test]
fn apply_worker_command_env_sets_voicepi_keys() {
    // F1 (Codex P1 PR #519 supervisor.rs:467): apply the WorkerCommand's
    // env vector to the process env so `load_settings()` and the real
    // backend constructors see the same view a Python child would inherit.
    // Uses the crate-wide ENV_LOCK because it mutates process env.
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());

    let sentinel_lang = "__vp_apply_env_test_lang__";
    let sentinel_prompt = "__vp_apply_env_test_prompt__";
    let previous_lang = std::env::var("VOICEPI_LANG").ok();
    let previous_prompt = std::env::var("VOICEPI_INITIAL_PROMPT").ok();
    let previous_python = std::env::var("PYTHONPATH").ok();
    let previous_ranger = std::env::var("VOICEPI_RUST_INJECTOR").ok();

    std::env::remove_var("VOICEPI_LANG");
    std::env::remove_var("VOICEPI_INITIAL_PROMPT");

    // Sentinel non-VOICEPI value that MUST NOT be applied.
    let pythonpath_marker = "__vp_apply_env_test_pythonpath__";
    std::env::remove_var("PYTHONPATH");

    let command = super::worker_command::WorkerCommand {
        program: std::path::PathBuf::from("python"),
        args: Vec::new(),
        working_dir: std::path::PathBuf::from("."),
        env: vec![
            ("PYTHONPATH".to_owned(), pythonpath_marker.to_owned()),
            ("VOICEPI_LANG".to_owned(), sentinel_lang.to_owned()),
            (
                "VOICEPI_INITIAL_PROMPT".to_owned(),
                sentinel_prompt.to_owned(),
            ),
            // The RUST_INJECTOR knob is meant for Python children only —
            // the in-process runtime injects directly. Applying it is
            // harmless but also unnecessary; the current impl includes
            // it since it starts with `VOICEPI_`. This assertion just
            // pins that PYTHONPATH is NOT applied.
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
        std::env::var("PYTHONPATH").ok().as_deref() != Some(pythonpath_marker),
        "PYTHONPATH must not be applied — child-only var"
    );

    // Restore every env var this test touched.
    match previous_lang {
        Some(v) => std::env::set_var("VOICEPI_LANG", v),
        None => std::env::remove_var("VOICEPI_LANG"),
    }
    match previous_prompt {
        Some(v) => std::env::set_var("VOICEPI_INITIAL_PROMPT", v),
        None => std::env::remove_var("VOICEPI_INITIAL_PROMPT"),
    }
    match previous_python {
        Some(v) => std::env::set_var("PYTHONPATH", v),
        None => std::env::remove_var("PYTHONPATH"),
    }
    match previous_ranger {
        Some(v) => std::env::set_var("VOICEPI_RUST_INJECTOR", v),
        None => std::env::remove_var("VOICEPI_RUST_INJECTOR"),
    }
}

#[test]
fn apply_worker_command_env_clobbers_existing_process_env() {
    // Matches Python child semantics: `WorkerCommand.env` overrides
    // process env for the child (via `.envs()` on the Command). The
    // in-process runtime must do the same so a config-file value
    // wins over a stale shell export -- otherwise a user with
    // `lang=da` in the config but a leftover `VOICEPI_LANG=en` in
    // their shell would see the wrong hint.
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let previous = std::env::var("VOICEPI_LANG").ok();

    std::env::set_var("VOICEPI_LANG", "stale-shell-export");

    let command = super::worker_command::WorkerCommand {
        program: std::path::PathBuf::from("python"),
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

    match previous {
        Some(v) => std::env::set_var("VOICEPI_LANG", v),
        None => std::env::remove_var("VOICEPI_LANG"),
    }
}
