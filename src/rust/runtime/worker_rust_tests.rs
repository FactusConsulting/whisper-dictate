//! Unit tests for the pure-logic helpers in [`super`]. The integration
//! test that actually spawns the binary lives in
//! `src/rust/tests/worker_rust_subprocess.rs` (Cargo integration test).
//!
//! What's exercised here:
//!
//! * the feature-gate / env-gate decision in
//!   [`super::should_delegate_to_worker_rust`];
//! * the stdin-command dispatcher table
//!   ([`super::dispatch_stdin_command`]);
//! * the runtime-event router ([`super::forward_event`]);
//! * the [`super::swap_command_to_worker_rust`] mutator.
//!
//! The thread-driven `WorkerRunner::run` mainloop is not exercised here
//! -- the integration test owns that path so we don't double-spawn the
//! binary inside cargo's own test runner.

use std::io::Cursor;
use std::sync::mpsc;
use std::time::Instant;

use serde_json::json;

use super::*;
use crate::hotkey::coordinator::{spawn as spawn_coordinator, CoordinatorThread, Mode, Options};
use crate::runtime::{RuntimeEvent, WorkerEvent};
use crate::test_env_lock::ENV_LOCK;

// ── feature gate ────────────────────────────────────────────────────────────

#[test]
fn all_required_features_enabled_reflects_cfg_set() {
    // The const reflects the build's actual feature set; pin both
    // branches so a misspelled cfg in the helper would fail this
    // test rather than silently making the whole gate inert.
    let expected = cfg!(all(
        feature = "whisper-rs-local",
        feature = "rust-injection",
        feature = "audio-in-rust",
        feature = "rust-hotkeys",
    ));
    assert_eq!(all_required_features_enabled(), expected);
}

/// Empty env vec — the pattern most tests here use since they only
/// exercise the process-env branches of `resolved_env`. Codex #441 P2
/// review round 3 introduced the env-vec parameter; for legibility
/// keep the "no override" case as a named constant.
const EMPTY_ENV: &[(String, String)] = &[];

#[test]
fn should_delegate_default_is_on_and_python_legacy_opts_out() {
    // Wave 5 PR 7 of #348 flipped the semantics: previously the gate
    // required `VOICEPI_DICTATE_BACKEND=rust-session` to opt IN; now
    // the Rust worker is the default and the user opts OUT via
    // `=python-legacy`. Feature-gated: the delegate can only be true
    // when the four-feature set is compiled in.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev_dictate = std::env::var_os("VOICEPI_DICTATE_BACKEND");
    let prev_hotkey = std::env::var_os("VOICEPI_HOTKEY_BACKEND");
    let prev_stt = std::env::var_os("VOICEPI_STT_BACKEND");
    // Clear the STT-backend gate so this test only exercises the
    // env+feature interaction. Wave 5.5 gap #1 added a third gate;
    // it's covered by dedicated tests below.
    std::env::remove_var("VOICEPI_STT_BACKEND");

    // env unset -> DEFAULT delegate (mirrors the feature gate). This
    // is the load-bearing PR 7 assertion: without any env var, the
    // gate returns true on a full-feature build so production hits
    // the Rust worker path.
    std::env::remove_var("VOICEPI_DICTATE_BACKEND");
    std::env::remove_var("VOICEPI_HOTKEY_BACKEND");
    assert_eq!(
        should_delegate_to_worker_rust(EMPTY_ENV),
        all_required_features_enabled(),
        "PR 7 default: unset VOICEPI_DICTATE_BACKEND must delegate on a full-feature build"
    );

    // env set to the historical `rust-session` opt-in -> still
    // delegate. The value is redundant post-PR-7 but recognised so
    // users with the setting exported in their profile keep the
    // exact same behaviour they had on PR 6.
    std::env::set_var("VOICEPI_DICTATE_BACKEND", "rust-session");
    assert_eq!(
        should_delegate_to_worker_rust(EMPTY_ENV),
        all_required_features_enabled(),
        "the historical `rust-session` value must still delegate post-PR-7"
    );

    // env set to `rust` (historical Python-side shell-out gate) ->
    // still delegate. That value is consumed by the Python wrapper
    // NOT by the Rust supervisor's delegate gate, so it must not
    // pull the supervisor off the new default.
    std::env::set_var("VOICEPI_DICTATE_BACKEND", "rust");
    assert_eq!(
        should_delegate_to_worker_rust(EMPTY_ENV),
        all_required_features_enabled(),
        "`rust` value is a Python-side knob and must not block delegation"
    );

    // env set to the escape hatch -> NEVER delegate, regardless of
    // features. This is the PR 7 rollback path: the supervisor
    // stays on the pre-PR-7 Python orchestrator.
    std::env::set_var("VOICEPI_DICTATE_BACKEND", "python-legacy");
    assert!(
        !should_delegate_to_worker_rust(EMPTY_ENV),
        "python-legacy escape hatch must opt out of delegation"
    );

    // Escape hatch matching is case-insensitive + trims whitespace
    // (shell-set values from a crash-cart edit must still work).
    std::env::set_var("VOICEPI_DICTATE_BACKEND", "  Python-Legacy  ");
    assert!(
        !should_delegate_to_worker_rust(EMPTY_ENV),
        "python-legacy escape hatch must match case-insensitively with surrounding whitespace"
    );

    // Claude review comment #3523185556 on PR #434 (preserved by PR
    // 7): `VOICEPI_HOTKEY_BACKEND` MUST NOT gate delegation; the
    // subprocess installs rdev on its own via `install_listener`
    // regardless of the hotkey-backend env var. If a user opted out
    // via python-legacy AND unset the hotkey backend, they must
    // still land on Python (belt-and-braces: hotkey env being unset
    // is the default state, so this also tests that the *combined*
    // gate is python-legacy-dominant).
    std::env::remove_var("VOICEPI_HOTKEY_BACKEND");
    std::env::set_var("VOICEPI_DICTATE_BACKEND", "python-legacy");
    assert!(
        !should_delegate_to_worker_rust(EMPTY_ENV),
        "escape hatch must dominate over any hotkey-backend state"
    );

    // And once we clear the escape hatch AND leave hotkey backend
    // unset, we're back on the new default -- the subprocess owns
    // its own rdev install so no separate opt-in is needed.
    std::env::remove_var("VOICEPI_DICTATE_BACKEND");
    assert_eq!(
        should_delegate_to_worker_rust(EMPTY_ENV),
        all_required_features_enabled(),
        "removing the escape hatch restores the PR 7 default even with hotkey backend unset"
    );

    // restore the previous env state so we don't pollute siblings.
    match prev_dictate {
        Some(v) => std::env::set_var("VOICEPI_DICTATE_BACKEND", v),
        None => std::env::remove_var("VOICEPI_DICTATE_BACKEND"),
    }
    match prev_hotkey {
        Some(v) => std::env::set_var("VOICEPI_HOTKEY_BACKEND", v),
        None => std::env::remove_var("VOICEPI_HOTKEY_BACKEND"),
    }
    match prev_stt {
        Some(v) => std::env::set_var("VOICEPI_STT_BACKEND", v),
        None => std::env::remove_var("VOICEPI_STT_BACKEND"),
    }
}

// ── Wave 5.5 gap #1: STT-backend gate ────────────────────────────────────────

/// Unset / whisper / faster-whisper / openai / groq are all supported
/// STT backends the rust-session worker knows how to build; the gate
/// must return None for each. Any other value must land in the
/// unsupported set so the supervisor stays on Python.
#[test]
fn unsupported_worker_rust_settings_reason_accepts_supported_backends() {
    // Codex #441 P2 round 3: the gate now consults an explicit env
    // vec (the supervisor's `effective_command.env`) so the STT-
    // backend check does not depend on process env. Pass each
    // supported value in the vec directly; no `ENV_LOCK` needed.
    for supported in ["", "whisper", "WHISPER", "faster-whisper", "openai", "GROQ"] {
        let env: Vec<(String, String)> = if supported.is_empty() {
            Vec::new()
        } else {
            vec![("VOICEPI_STT_BACKEND".to_owned(), supported.to_owned())]
        };
        assert!(
            unsupported_worker_rust_settings_reason(&env).is_none(),
            "value {supported:?} must be a supported STT backend"
        );
    }
}

/// An unrecognised STT backend value (stale parakeet, typo, ...) must
/// surface a Some(reason) so the supervisor logs a clear message and
/// falls back to the Python worker.
#[test]
fn unsupported_worker_rust_settings_reason_rejects_unknown_backend() {
    let env = vec![(
        "VOICEPI_STT_BACKEND".to_owned(),
        "parakeet".to_owned(),
    )];
    let reason = unsupported_worker_rust_settings_reason(&env).expect("parakeet is unsupported");
    assert!(
        reason.contains("parakeet"),
        "reason must name the offending value: {reason}"
    );
    assert!(
        reason.contains("staying on Python"),
        "reason must explain the fallback: {reason}"
    );
}

/// Codex #441 P2 round 3: an `stt_backend = "parakeet"` value that
/// only appears in the supervisor's `effective_command.env` (materialised
/// from AppSettings by `worker_env_overrides`) MUST veto delegation
/// even when the parent process env has no `VOICEPI_STT_BACKEND` set.
/// This is the load-bearing regression the round-3 signature change
/// closes: previously the gate consulted `std::env::var` and missed
/// config-file overrides.
#[test]
fn unsupported_worker_rust_settings_reason_reads_effective_command_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var_os("VOICEPI_STT_BACKEND");
    // Parent process env: not set. If the gate were still reading
    // std::env alone it would see this as an "unset -> supported"
    // and let the child through.
    std::env::remove_var("VOICEPI_STT_BACKEND");

    let effective = vec![(
        "VOICEPI_STT_BACKEND".to_owned(),
        "parakeet".to_owned(),
    )];
    let reason =
        unsupported_worker_rust_settings_reason(&effective).expect("effective env must veto");
    assert!(
        reason.contains("parakeet"),
        "reason must name the offending value: {reason}"
    );

    match prev {
        Some(v) => std::env::set_var("VOICEPI_STT_BACKEND", v),
        None => std::env::remove_var("VOICEPI_STT_BACKEND"),
    }
}

/// The env vec wins over `std::env` when both carry a value. Mirrors
/// the child's actual view (`Command::envs` overrides inherited env).
#[test]
fn resolved_env_prefers_effective_command_env_over_process_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var_os("VOICEPI_STT_BACKEND");
    std::env::set_var("VOICEPI_STT_BACKEND", "whisper");
    let effective = vec![(
        "VOICEPI_STT_BACKEND".to_owned(),
        "parakeet".to_owned(),
    )];
    // Effective override wins -> reason surfaces.
    assert!(unsupported_worker_rust_settings_reason(&effective).is_some());
    // Fallback to process env when key is absent from the vec ->
    // supported.
    let empty: &[(String, String)] = &[];
    assert!(unsupported_worker_rust_settings_reason(empty).is_none());
    match prev {
        Some(v) => std::env::set_var("VOICEPI_STT_BACKEND", v),
        None => std::env::remove_var("VOICEPI_STT_BACKEND"),
    }
}

/// End-to-end delegate gate: with the dictate-backend env, feature
/// set, AND stt_backend=openai/groq all lined up, delegation fires so
/// the supervisor swaps the command to worker-rust instead of Python.
/// Wave 5.5 gap #1 -- before this PR delegation happened but the
/// worker couldn't build the cloud backend, silently producing empty
/// no_text events; the enum-based factory + config-gate together
/// close the gap.
#[test]
fn should_delegate_fires_for_cloud_stt_backends_when_features_present() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev_dictate = std::env::var_os("VOICEPI_DICTATE_BACKEND");

    std::env::set_var("VOICEPI_DICTATE_BACKEND", "rust-session");
    for cloud in ["openai", "groq"] {
        let env = vec![("VOICEPI_STT_BACKEND".to_owned(), cloud.to_owned())];
        assert_eq!(
            should_delegate_to_worker_rust(&env),
            all_required_features_enabled(),
            "delegation must not refuse stt_backend={cloud:?}"
        );
    }

    // And the unsupported-value branch must still veto delegation.
    let env = vec![(
        "VOICEPI_STT_BACKEND".to_owned(),
        "parakeet".to_owned(),
    )];
    assert!(
        !should_delegate_to_worker_rust(&env),
        "unsupported stt_backend must veto delegation"
    );

    match prev_dictate {
        Some(v) => std::env::set_var("VOICEPI_DICTATE_BACKEND", v),
        None => std::env::remove_var("VOICEPI_DICTATE_BACKEND"),
    }
}

// ── command swap ────────────────────────────────────────────────────────────

#[test]
fn swap_command_replaces_program_and_args_in_place() {
    // Build a representative Python-orchestrator WorkerCommand the
    // supervisor would have produced via worker_command_with_args.
    let mut cmd = WorkerCommand {
        program: std::path::PathBuf::from("python3"),
        args: vec![
            "-m".to_owned(),
            "whisper_dictate.runtime".to_owned(),
            "--app-root".to_owned(),
            "/tmp/wd".to_owned(),
        ],
        working_dir: std::path::PathBuf::from("/tmp/wd"),
        env: vec![("VOICEPI_KEY".to_owned(), "ctrl_l".to_owned())],
    };
    let original_env = cmd.env.clone();

    swap_command_to_worker_rust(&mut cmd).expect("current_exe must resolve in tests");

    // Program now points at the current test binary; args reduce to
    // just the subcommand name. Env carries over untouched so the
    // worker subprocess sees the same VOICEPI_* knobs the Python
    // worker would have.
    assert!(
        cmd.program.exists(),
        "swapped program should be a real path (current_exe), got: {}",
        cmd.program.display()
    );
    assert_eq!(cmd.args, vec!["worker-rust".to_owned()]);
    assert_eq!(cmd.env, original_env);
}

// ── plan_worker_rust_delegation ─────────────────────────────────────────────
//
// Claude review comment #3523185636 on PR #434: on `swap_command_to_worker_rust`
// failure, the supervisor MUST reset delegate + honour the original
// audio-backend gate together. These tests pin the composed decision
// so a future refactor that splits them back cannot land silently.

#[test]
fn delegation_plan_delegates_when_requested_and_swap_ok() {
    // Happy path: user opted in, swap succeeded. Subprocess owns
    // audio -- no `--audio-source=rust-stdin` push on the (now
    // worker-rust) command.
    let plan = plan_worker_rust_delegation(true, true, true);
    assert_eq!(
        plan,
        DelegatePlan {
            delegate: true,
            push_rust_stdin_arg: false,
        }
    );
    // Same happy path with rust-audio NOT requested.
    let plan = plan_worker_rust_delegation(true, true, false);
    assert_eq!(
        plan,
        DelegatePlan {
            delegate: true,
            push_rust_stdin_arg: false,
        }
    );
}

#[test]
fn delegation_plan_falls_back_to_python_when_swap_fails_preserving_audio_arg() {
    // The load-bearing case for Claude review comment #3523185636:
    // delegate requested but swap failed AND rust-audio requested.
    // Falls back to Python AND pushes --audio-source=rust-stdin, so
    // the child reads from the audio bridge instead of stalling on
    // an unread pipe.
    let plan = plan_worker_rust_delegation(true, false, true);
    assert_eq!(
        plan,
        DelegatePlan {
            delegate: false,
            push_rust_stdin_arg: true,
        }
    );
    // Same failure path with rust-audio NOT requested: fall back to
    // Python without pushing the arg (Python does its own capture).
    let plan = plan_worker_rust_delegation(true, false, false);
    assert_eq!(
        plan,
        DelegatePlan {
            delegate: false,
            push_rust_stdin_arg: false,
        }
    );
}

#[test]
fn delegation_plan_stays_on_python_when_not_requested() {
    // User did not opt in. `swap_succeeded` is irrelevant on this
    // branch (the supervisor never tried the swap). The plan should
    // still honour the `rust_audio_requested` gate so
    // VOICEPI_AUDIO_BACKEND=rust keeps working on the Python path.
    let plan = plan_worker_rust_delegation(false, false, true);
    assert_eq!(
        plan,
        DelegatePlan {
            delegate: false,
            push_rust_stdin_arg: true,
        }
    );
    let plan = plan_worker_rust_delegation(false, true, false);
    assert_eq!(
        plan,
        DelegatePlan {
            delegate: false,
            push_rust_stdin_arg: false,
        }
    );
}

// ── stdin command dispatcher ────────────────────────────────────────────────

/// Tag the coordinator output as a `PartialEq`-able variant so the
/// dispatcher tests can pin "press produced StartRecording" etc.
/// without leaking implementation detail of the coordinator's id
/// numbering into the assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedAction {
    Start,
    Stop,
    Cancel,
}

/// Spawn a coordinator that maps each emitted action onto an
/// [`ObservedAction`] on `rx`. Returns the handle, the channel, and
/// the join handle so each test cleanly shuts the coordinator down on
/// exit (tests must not leak threads -- the coordinator's
/// `recv_timeout` loop only exits on `Shutdown`).
fn spawn_observer() -> (
    CoordinatorHandle,
    mpsc::Receiver<ObservedAction>,
    CoordinatorThread,
) {
    let (action_tx, action_rx) = mpsc::channel();
    let (handle, thread) = spawn_coordinator(
        Options {
            mode: Mode::HoldToTalk,
        },
        move |action| {
            use crate::hotkey::coordinator::CoordinatorAction;
            let tag = match action {
                CoordinatorAction::StartRecording(_) => ObservedAction::Start,
                CoordinatorAction::StopAndTranscribe(_) => ObservedAction::Stop,
                CoordinatorAction::CancelRecording(_) => ObservedAction::Cancel,
            };
            let _ = action_tx.send(tag);
        },
        Instant::now,
    );
    (handle, action_rx, thread)
}

/// Drain every queued ObservedAction without blocking. Replaces the
/// earlier `drain_coord_events` helper now that we work in
/// ObservedAction space (we can't peek at raw CoordinatorEvents in
/// flight on the handle's private sender).
fn drain_actions(rx: &mpsc::Receiver<ObservedAction>) -> Vec<ObservedAction> {
    let mut out = Vec::new();
    while let Ok(e) = rx.try_recv() {
        out.push(e);
    }
    out
}

#[test]
fn dispatch_empty_line_is_a_noop() {
    let (coord, rx, thread) = spawn_observer();
    let outcome = dispatch_stdin_command("", &coord);
    assert_eq!(outcome, StdinCommandOutcome::Continue);
    // Coordinator never even saw the line, so no action fires.
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(drain_actions(&rx).is_empty());
    coord.shutdown();
    thread.join();
}

#[test]
fn dispatch_press_release_drives_coordinator_actions_in_order() {
    let (coord, rx, thread) = spawn_observer();
    assert_eq!(
        dispatch_stdin_command("press", &coord),
        StdinCommandOutcome::Continue
    );
    assert_eq!(
        dispatch_stdin_command("release", &coord),
        StdinCommandOutcome::Continue
    );
    // Give the coordinator thread a moment to emit the actions.
    std::thread::sleep(std::time::Duration::from_millis(150));
    let events = drain_actions(&rx);
    assert_eq!(
        events,
        vec![ObservedAction::Start, ObservedAction::Stop],
        "press then release must drive StartRecording then StopAndTranscribe"
    );
    coord.shutdown();
    thread.join();
}

#[test]
fn dispatch_cancel_drives_coordinator_cancel_action() {
    let (coord, rx, thread) = spawn_observer();
    // Get the coordinator into Recording first so the Cancel produces
    // an action (cancel from Idle is silently dropped by the
    // coordinator's state machine).
    assert_eq!(
        dispatch_stdin_command("press", &coord),
        StdinCommandOutcome::Continue
    );
    std::thread::sleep(std::time::Duration::from_millis(80));
    let _ = drain_actions(&rx);

    assert_eq!(
        dispatch_stdin_command("cancel", &coord),
        StdinCommandOutcome::Continue
    );
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert_eq!(drain_actions(&rx), vec![ObservedAction::Cancel]);
    coord.shutdown();
    thread.join();
}

#[test]
fn dispatch_quit_and_exit_terminate_loop() {
    let (coord, _rx, thread) = spawn_observer();
    assert_eq!(
        dispatch_stdin_command("quit", &coord),
        StdinCommandOutcome::Quit
    );
    assert_eq!(
        dispatch_stdin_command("exit", &coord),
        StdinCommandOutcome::Quit
    );
    coord.shutdown();
    thread.join();
}

#[test]
fn dispatch_unknown_command_continues() {
    let (coord, _rx, thread) = spawn_observer();
    // Unknown commands must not abort the loop -- they print a stderr
    // warning and continue (so a stray newline / typo on the
    // supervisor side doesn't take the worker down mid-session).
    assert_eq!(
        dispatch_stdin_command("not-a-command", &coord),
        StdinCommandOutcome::Continue
    );
    coord.shutdown();
    thread.join();
}

// ── event forwarder ─────────────────────────────────────────────────────────

#[test]
fn forward_worker_event_emits_canonical_wire_line_on_stderr() {
    let payload = json!({
        "event": "status",
        "state": "recording",
        "capture_backend": "rust-stdin",
    });
    let event = RuntimeEvent::Worker(WorkerEvent {
        event: "status".to_owned(),
        state: Some("recording".to_owned()),
        payload,
    });

    let mut stderr = Cursor::new(Vec::new());
    let mut stdout = Cursor::new(Vec::new());
    forward_event(event, &mut stderr, &mut stdout);

    let stderr_text = String::from_utf8(stderr.into_inner()).unwrap();
    assert!(
        stderr_text.starts_with("[worker-event] "),
        "worker events must round-trip with the wire prefix; got: {stderr_text:?}"
    );
    assert!(
        stderr_text.contains("\"state\":\"recording\""),
        "payload fields must survive the round-trip; got: {stderr_text:?}"
    );
    assert!(
        stderr_text.ends_with('\n'),
        "worker-event lines must terminate with `\\n` so the parent supervisor's \
         line-buffered parse_worker_event consumer ingests them"
    );

    assert!(
        stdout.into_inner().is_empty(),
        "worker events must NOT bleed onto stdout"
    );
}

#[test]
fn forward_stderr_writes_to_stderr_without_prefix() {
    let event = RuntimeEvent::Stderr("hello world".to_owned());
    let mut stderr = Cursor::new(Vec::new());
    let mut stdout = Cursor::new(Vec::new());
    forward_event(event, &mut stderr, &mut stdout);
    assert_eq!(stderr.into_inner(), b"hello world\n");
    assert!(stdout.into_inner().is_empty());
}

#[test]
fn forward_error_carries_worker_rust_prefix() {
    let event = RuntimeEvent::Error("session failed".to_owned());
    let mut stderr = Cursor::new(Vec::new());
    let mut stdout = Cursor::new(Vec::new());
    forward_event(event, &mut stderr, &mut stdout);
    assert_eq!(
        stderr.into_inner(),
        b"[worker-rust] error: session failed\n"
    );
}

#[test]
fn forward_stdout_writes_to_stdout() {
    let event = RuntimeEvent::Stdout("user-visible line".to_owned());
    let mut stderr = Cursor::new(Vec::new());
    let mut stdout = Cursor::new(Vec::new());
    forward_event(event, &mut stderr, &mut stdout);
    assert!(stderr.into_inner().is_empty());
    assert_eq!(stdout.into_inner(), b"user-visible line\n");
}

#[test]
fn forward_supervisor_only_variants_are_dropped_silently() {
    // RuntimeEvent::Started / RuntimeEvent::Exited are never emitted
    // by the session sink (they're the supervisor's own bookkeeping).
    // The forwarder treats them as no-ops so the unreachable branch
    // doesn't accidentally write garbage to stderr.
    let mut stderr = Cursor::new(Vec::new());
    let mut stdout = Cursor::new(Vec::new());
    forward_event(
        RuntimeEvent::Started {
            command: "x".to_owned(),
        },
        &mut stderr,
        &mut stdout,
    );
    forward_event(
        RuntimeEvent::Exited { code: Some(0) },
        &mut stderr,
        &mut stdout,
    );
    assert!(stderr.into_inner().is_empty());
    assert!(stdout.into_inner().is_empty());
}

// ── emit_worker_ready ───────────────────────────────────────────────────────

#[test]
fn emit_worker_ready_writes_a_status_ready_line() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Worker-event emission is gated on VOICEPI_WORKER_EVENTS; toggle
    // it on for the test (the production handle_worker_rust does the
    // same) so the helper actually writes.
    let prev = std::env::var_os("VOICEPI_WORKER_EVENTS");
    std::env::set_var("VOICEPI_WORKER_EVENTS", "1");

    let mut buf = Cursor::new(Vec::new());
    emit_worker_ready(&mut buf).expect("emit_worker_ready writes to a Cursor");
    let text = String::from_utf8(buf.into_inner()).unwrap();

    assert!(text.starts_with("[worker-event] "), "got: {text:?}");
    assert!(
        text.contains("\"state\":\"ready\""),
        "ready event must carry state=ready; got: {text:?}"
    );

    match prev {
        Some(v) => std::env::set_var("VOICEPI_WORKER_EVENTS", v),
        None => std::env::remove_var("VOICEPI_WORKER_EVENTS"),
    }
}
