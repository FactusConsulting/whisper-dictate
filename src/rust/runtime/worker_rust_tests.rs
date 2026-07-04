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

#[test]
fn should_delegate_only_when_env_and_features_match() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev_dictate = std::env::var_os("VOICEPI_DICTATE_BACKEND");
    let prev_hotkey = std::env::var_os("VOICEPI_HOTKEY_BACKEND");

    // env unset -> never delegate, regardless of features.
    std::env::remove_var("VOICEPI_DICTATE_BACKEND");
    std::env::remove_var("VOICEPI_HOTKEY_BACKEND");
    assert!(!should_delegate_to_worker_rust());

    // env set but not the magic value -> never delegate.
    std::env::set_var("VOICEPI_DICTATE_BACKEND", "rust");
    assert!(!should_delegate_to_worker_rust());

    // env set to the magic value -> result mirrors the feature gate.
    std::env::set_var("VOICEPI_DICTATE_BACKEND", "rust-session");
    assert_eq!(
        should_delegate_to_worker_rust(),
        all_required_features_enabled()
    );

    // Claude review comment #3523185556 on PR #434: even with
    // `VOICEPI_HOTKEY_BACKEND` unset the delegate gate MUST still
    // fire so the supervisor spawns the worker-rust subprocess (which
    // owns the hotkey install internally via `install_listener` --
    // that helper bypasses the separate hotkey-backend env var
    // because the subprocess IS the requested backend by
    // construction). Otherwise a user who set only
    // VOICEPI_DICTATE_BACKEND=rust-session would silently regress to
    // Python.
    std::env::remove_var("VOICEPI_HOTKEY_BACKEND");
    assert_eq!(
        should_delegate_to_worker_rust(),
        all_required_features_enabled(),
        "delegate gate must not require VOICEPI_HOTKEY_BACKEND -- \
         the subprocess installs rdev unconditionally on the delegate path"
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
