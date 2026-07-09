//! Sibling tests for the `run_terminal` foreground delegate hook
//! (`apply_worker_rust_delegation_to_foreground`).
//!
//! Codex #441 P2 review round 3 finding 6 ("Route foreground dictation
//! through worker-rust too"). The `whisper-dictate run` entry point did
//! not consult the delegate gate that `RuntimeSupervisor::start` applies
//! for the UI-launched supervisor path, so on Wave-8 (Python bundle
//! removed) full-feature installs the foreground command would fail
//! with a confusing "no Python" error rather than running the
//! in-process Rust worker.
//!
//! These tests pin the pure-logic branch decision so a future refactor
//! that re-splits the swap decision from the delegate gate cannot land
//! silently. The tests do NOT spawn a subprocess -- they exercise
//! `apply_worker_rust_delegation_to_foreground` on a synthetic
//! [`WorkerCommand`] and assert program/args mutation.

use super::test_support::ENV_LOCK;
use super::worker_rust::all_required_features_enabled;
use super::*;

/// Build a representative Python-orchestrator command matching what
/// `default_worker_command_with_args` used to hand to `run_foreground`
/// before the delegate hook was inlined. Tests mutate this in place
/// and assert whether the swap fired.
fn python_command_with_env(env: Vec<(String, String)>) -> WorkerCommand {
    WorkerCommand {
        program: std::path::PathBuf::from("python3"),
        args: vec![
            "-m".to_owned(),
            "whisper_dictate.runtime".to_owned(),
            "--app-root".to_owned(),
            "/tmp/wd".to_owned(),
        ],
        working_dir: std::path::PathBuf::from("/tmp/wd"),
        env,
    }
}

#[test]
fn foreground_delegate_swaps_command_when_gate_approves() {
    // Full-feature builds default-delegate on an empty env. On stock CI
    // (feature-off) the gate short-circuits on `all_required_features_enabled`
    // so this test is a no-op for that branch -- pinned via an `if`
    // rather than a `#[cfg]` so the compiled test binary still contains
    // the assertion when features flip locally.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Clear any escape-hatch env leaked in by a sibling test that
    // panicked mid-flight -- the ENV_LOCK guarantees no other test
    // mutates env alongside us, but the `set_var`/`remove_var` restore
    // pair is only guaranteed on drop, not on panic before drop.
    let prev = std::env::var_os("VOICEPI_DICTATE_BACKEND");
    std::env::remove_var("VOICEPI_DICTATE_BACKEND");
    // Also clear the STT-backend leak -- an unrecognised value would
    // veto delegation regardless of features.
    let prev_stt = std::env::var_os("VOICEPI_STT_BACKEND");
    std::env::remove_var("VOICEPI_STT_BACKEND");
    // Codex #441 finding 2 composes with finding 6: the delegate gate
    // now vetoes when no local Whisper model resolves. Set the path to
    // a dummy non-empty value so the gate's "explicit override" branch
    // fires -- existence is only checked at `LocalWhisper::new` time.
    let prev_model = std::env::var_os("VOICEPI_WHISPER_MODEL_PATH");
    std::env::set_var("VOICEPI_WHISPER_MODEL_PATH", "/tmp/dummy-model.bin");

    let mut command = python_command_with_env(Vec::new());
    apply_worker_rust_delegation_to_foreground(&mut command);

    if all_required_features_enabled() {
        // Feature-full build: the swap must have fired, so `program`
        // now points at the current test binary and `args` reduce to
        // just the subcommand name. Env is preserved by the swap.
        assert!(
            command.program.exists(),
            "swapped program should be a real path (current_exe), got: {}",
            command.program.display()
        );
        assert_eq!(command.args, vec!["worker-rust".to_owned()]);
    } else {
        // Feature-off build: gate short-circuits, command is unchanged.
        assert_eq!(command.program, std::path::PathBuf::from("python3"));
        assert_eq!(
            command.args,
            vec![
                "-m".to_owned(),
                "whisper_dictate.runtime".to_owned(),
                "--app-root".to_owned(),
                "/tmp/wd".to_owned(),
            ]
        );
    }

    match prev {
        Some(v) => std::env::set_var("VOICEPI_DICTATE_BACKEND", v),
        None => std::env::remove_var("VOICEPI_DICTATE_BACKEND"),
    }
    match prev_stt {
        Some(v) => std::env::set_var("VOICEPI_STT_BACKEND", v),
        None => std::env::remove_var("VOICEPI_STT_BACKEND"),
    }
    match prev_model {
        Some(v) => std::env::set_var("VOICEPI_WHISPER_MODEL_PATH", v),
        None => std::env::remove_var("VOICEPI_WHISPER_MODEL_PATH"),
    }
}

#[test]
fn foreground_delegate_leaves_command_alone_on_python_legacy_escape_hatch() {
    // Escape hatch is a process-env-only knob (per its docs); the gate
    // reads std::env unconditionally. Set it and confirm the command
    // is untouched even on a full-feature build.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var_os("VOICEPI_DICTATE_BACKEND");
    std::env::set_var("VOICEPI_DICTATE_BACKEND", "python-legacy");

    let mut command = python_command_with_env(Vec::new());
    let before_program = command.program.clone();
    let before_args = command.args.clone();
    apply_worker_rust_delegation_to_foreground(&mut command);
    assert_eq!(command.program, before_program);
    assert_eq!(command.args, before_args);

    match prev {
        Some(v) => std::env::set_var("VOICEPI_DICTATE_BACKEND", v),
        None => std::env::remove_var("VOICEPI_DICTATE_BACKEND"),
    }
}

#[test]
fn foreground_delegate_leaves_command_alone_on_unsupported_stt_backend_in_effective_env() {
    // Codex #441 finding 6 composes with finding 3 (`Honor saved STT
    // backend before delegating`): the gate consults the effective
    // command env FIRST, so an upgraded `stt_backend = "parakeet"`
    // in `effective_command.env` must veto the foreground swap the
    // same way it vetoes the supervisor path.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_stt = std::env::var_os("VOICEPI_STT_BACKEND");
    std::env::remove_var("VOICEPI_STT_BACKEND");
    let prev_dict = std::env::var_os("VOICEPI_DICTATE_BACKEND");
    std::env::remove_var("VOICEPI_DICTATE_BACKEND");

    let mut command = python_command_with_env(vec![(
        "VOICEPI_STT_BACKEND".to_owned(),
        "parakeet".to_owned(),
    )]);
    let before_program = command.program.clone();
    let before_args = command.args.clone();
    apply_worker_rust_delegation_to_foreground(&mut command);
    assert_eq!(command.program, before_program);
    assert_eq!(command.args, before_args);

    match prev_stt {
        Some(v) => std::env::set_var("VOICEPI_STT_BACKEND", v),
        None => std::env::remove_var("VOICEPI_STT_BACKEND"),
    }
    match prev_dict {
        Some(v) => std::env::set_var("VOICEPI_DICTATE_BACKEND", v),
        None => std::env::remove_var("VOICEPI_DICTATE_BACKEND"),
    }
}
