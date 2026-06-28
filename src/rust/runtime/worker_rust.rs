//! Wave 5 PR 6 of #348: the `whisper-dictate worker-rust` CLI entry
//! point — a long-running, in-process Rust dictation worker that
//! replaces the Python `vp_dictate.py` / `runtime.py` orchestrator
//! when `VOICEPI_DICTATE_BACKEND=rust-session` is set AND the binary
//! was built with the full feature set
//! (`whisper-rs-local,rust-injection,audio-in-rust,rust-hotkeys`).
//!
//! The subprocess driven by this entry point owns the full dictation
//! lifecycle: the hotkey coordinator, the production
//! [`crate::dictate::DictateSession`] (via
//! [`super::rust_session_sink::build_production_sink`]), the rdev OS
//! listener (skipped on `--stdin-only`), a stdin command parser
//! (`press` / `release` / `cancel` / `quit`), and a foreground event-
//! pump that forwards [`RuntimeEvent`]s to stderr/stdout in the same
//! wire format the supervisor's `parse_worker_event` consumer already
//! ingests.
//!
//! # PR 6 is opt-in only
//!
//! * `VOICEPI_DICTATE_BACKEND` unset -> supervisor spawns Python
//!   (byte-for-byte unchanged behaviour).
//! * `VOICEPI_DICTATE_BACKEND=rust-session` + all four features ->
//!   supervisor spawns this subcommand instead of Python.
//! * `VOICEPI_DICTATE_BACKEND=rust-session` + any feature missing ->
//!   supervisor stays on Python (this subcommand exits non-zero with
//!   a clear "feature not compiled in" message if invoked directly).
//!
//! PR 7 will delete `vp_dictate.py` / `runtime.py` and flip the
//! default; until then production keeps shipping Python.
//!
//! # Test mode (`--stdin-only`)
//!
//! Stock-feature CI cannot install rdev (no display / permission), so
//! the hidden `--stdin-only` flag skips the rdev install and drives
//! the coordinator from stdin commands. The session sink is still
//! the real one (or the PR 4 stub on stock builds) so the
//! coordinator -> sink -> session -> emitter chain is exercised end-
//! to-end. EOF on stdin is treated as `quit` so the supervisor can
//! shut the worker down by closing its pipe end (the portable
//! shutdown mechanism on Windows where SIGTERM isn't reliable).
//!
//! Integration test lives at
//! `src/rust/tests/worker_rust_subprocess.rs` (Cargo integration
//! test) because it spawns the binary -- `env!("CARGO_BIN_EXE_*")`
//! is only defined for integration tests.

use std::io::{self, BufRead, BufReader, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use anyhow::{anyhow, Result};

use crate::dictate::events as dictate_events;
use crate::hotkey::coordinator::{
    spawn as spawn_coordinator, CoordinatorEvent, CoordinatorHandle, Mode, Options,
};
use crate::runtime::{
    extract_hotkey_key_names, parse_toggle_value, rust_session_sink, RuntimeEvent, WorkerCommand,
};

#[cfg(test)]
#[path = "worker_rust_tests.rs"]
mod tests;

/// True when this build was compiled with every feature the production
/// rust-session worker needs. The supervisor uses this -- together with
/// [`rust_session_sink::dictate_backend_rust_session_requested`] -- to
/// decide whether to delegate to the worker-rust subprocess instead of
/// the Python orchestrator. A `--stdin-only` test build bypasses the
/// audio pump + rdev install, but the production sink helper still
/// expects all four features to be on.
///
/// All four flags are required:
///
/// * `whisper-rs-local` -- the real Whisper backend.
/// * `rust-injection` -- the real OS injector (enigo).
/// * `audio-in-rust` -- the cpal audio pump.
/// * `rust-hotkeys` -- the rdev OS hotkey listener.
///
/// Without any one of them, the production rust-session path cannot
/// drive a real dictation; the supervisor stays on Python.
pub const fn all_required_features_enabled() -> bool {
    cfg!(all(
        feature = "whisper-rs-local",
        feature = "rust-injection",
        feature = "audio-in-rust",
        feature = "rust-hotkeys",
    ))
}

/// Whether the supervisor should delegate the dictation lifecycle to
/// the worker-rust subprocess instead of Python. True iff both:
///
/// * the user requested the rust-session backend via env var, AND
/// * the binary was built with [`all_required_features_enabled`].
///
/// Pure helper so the gate is unit-testable. The supervisor's
/// `RuntimeSupervisor::start` consults this to swap its spawned
/// command for the worker-rust subcommand AND to skip its own
/// in-process Rust hotkey install (the subprocess installs its own).
pub fn should_delegate_to_worker_rust() -> bool {
    rust_session_sink::dictate_backend_rust_session_requested() && all_required_features_enabled()
}

/// Swap an existing Python-orchestrator [`WorkerCommand`] in place so
/// it spawns `<current-exe> worker-rust` instead. Preserves the
/// existing env (`VOICEPI_KEY`, `VOICEPI_TOGGLE`, `VOICEPI_LANG`,
/// model paths, ...) so the worker subprocess sees exactly the same
/// runtime configuration the Python worker would have seen.
///
/// `--stdin-only` is NOT added here -- production builds want the
/// real rdev listener. The integration test invokes the subcommand
/// with the flag explicitly.
///
/// Fails when `std::env::current_exe()` cannot be resolved (e.g. the
/// binary was deleted out from under us mid-run). The supervisor
/// surfaces the error and stays on Python in that case.
pub fn swap_command_to_worker_rust(command: &mut WorkerCommand) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow!("cannot resolve current executable for worker-rust: {e}"))?;
    command.program = exe;
    command.args = vec!["worker-rust".to_owned()];
    Ok(())
}

/// Public CLI entry point. Wired from `src/rust/main.rs`'s clap
/// dispatcher. On stock builds (any feature missing) returns a clear
/// error so the caller -- usually the supervisor's launched
/// subprocess -- exits non-zero and the parent UI shows a useful
/// message rather than a silent hang.
///
/// `stdin_only=true` skips the rdev install (for the integration test
/// running in headless CI). Production callers leave it unset.
pub fn handle_worker_rust(stdin_only: bool) -> Result<()> {
    if !all_required_features_enabled() && !stdin_only {
        return Err(anyhow!(
            "this build of whisper-dictate was compiled without the full \
             rust-session feature set; rebuild with \
             `--features whisper-rs-local,rust-injection,audio-in-rust,rust-hotkeys` \
             to enable the in-process Rust dictation worker, OR pass \
             `--stdin-only` to run the test-driven session sink (stub \
             backends, no OS hotkey listener)"
        ));
    }
    // Always enable the worker-event gate for this process so the
    // session's emitter (which the supervisor's `parse_worker_event`
    // ingests) actually writes. Mirrors what
    // `build_production_sink` already does on its own -- doing it
    // here too lets us emit the startup `ready` event BEFORE the sink
    // is built, so the supervisor's parse loop sees a heartbeat even
    // if the sink construction itself fails.
    std::env::set_var(dictate_events::WORKER_EVENTS_ENV, "1");

    let runner = WorkerRunner::from_env(stdin_only)?;
    runner.run()
}

/// Bundle of runtime configuration that the worker thread needs.
/// Captured up-front from env vars / CLI flags so the main loop never
/// re-reads the env mid-flight (the values are also passed down to
/// the coordinator + session sink via the existing wiring -- this
/// struct is a thin local cache for the bits the worker mainloop
/// itself consults).
struct WorkerRunner {
    /// Pre-parsed PTT chord names from `VOICEPI_KEY` (split on `+`,
    /// trimmed, empties dropped). Empty when no chord is configured;
    /// production builds without a chord refuse to install the rdev
    /// listener (PTT is silent otherwise), test builds tolerate it
    /// because they drive the coordinator from stdin.
    key_names: Vec<String>,
    /// Hold-to-talk vs. toggle mode parsed from `VOICEPI_TOGGLE`.
    mode: Mode,
    /// When true, skip the rdev install and drive the coordinator
    /// from stdin only. Set by the integration test.
    stdin_only: bool,
}

impl WorkerRunner {
    fn from_env(stdin_only: bool) -> Result<Self> {
        // Re-use the supervisor's existing helpers so the env-var
        // semantics stay in lock-step with the Python orchestrator
        // path (one source of truth per knob).
        let dummy = WorkerCommand {
            program: std::path::PathBuf::new(),
            args: Vec::new(),
            working_dir: std::path::PathBuf::new(),
            env: std::env::vars().collect(),
        };
        let key_names = extract_hotkey_key_names(&dummy);
        let toggle = std::env::var("VOICEPI_TOGGLE")
            .ok()
            .as_deref()
            .map(parse_toggle_value)
            .unwrap_or(false);
        let mode = if toggle {
            Mode::Toggle
        } else {
            Mode::HoldToTalk
        };
        Ok(Self {
            key_names,
            mode,
            stdin_only,
        })
    }

    /// Drive the worker mainloop until stdin EOF / `quit` / fatal error.
    fn run(self) -> Result<()> {
        let stderr = io::stderr();

        // Emit a heartbeat the supervisor's `parse_worker_event` consumer
        // can latch onto BEFORE the session sink builds. Without this,
        // the first observable worker event would come from the session
        // itself (no startup ready), so the supervisor's `state=ready`
        // wait (audio bridge / UI status badge) would not fire until the
        // first PTT press. Matches the Python orchestrator's behaviour
        // (`runtime.py` emits `status=ready` once the model is loaded).
        emit_worker_ready(&mut stderr.lock())?;

        // Build the production session sink. On a stock build this
        // falls back to the PR 4 stubs (transcribe -> empty text,
        // inject -> no-op) and there's no audio pump; production
        // builds get the real backends + cpal pump.
        let (tx, rx) = mpsc::channel::<RuntimeEvent>();
        let (action_sink, coord_slot) = rust_session_sink::build_production_sink(tx.clone(), None);

        // Spawn the coordinator with the session sink as its action sink.
        let (coord_handle, coord_thread) =
            spawn_coordinator(Options { mode: self.mode }, action_sink, Instant::now);
        // Make the coordinator handle available to the sink so its
        // `processing_finished` callback can drive the coordinator out
        // of `Stage::Processing` after a stop completes. `OnceLock::set`
        // returns Err on a second call -- we are the only writer so
        // this is unreachable in practice, but match the supervisor's
        // belt-and-braces path.
        if coord_slot.set(coord_handle.clone()).is_err() {
            eprintln!(
                "[worker-rust] coordinator slot already populated; ignoring \
                 (programming error if this fires more than once)"
            );
        }

        // Optionally install the rdev OS listener. Skipped on
        // `--stdin-only` (integration test) and on stock builds where
        // the `rust-hotkeys` feature is off (the `maybe_install_rust_hotkey`
        // helper handles the feature gate internally -- we still call
        // through it so the env-var-set + feature-off warning fires).
        let hotkey_handle = if self.stdin_only {
            None
        } else {
            install_listener(self.key_names.clone(), self.mode, coord_handle.clone())
        };

        // Pump events from the channel to stderr/stdout in a background
        // thread so the main thread is free to read stdin commands.
        let pump = spawn_event_pump(rx);

        // Drive stdin commands on the main thread. EOF / `quit` returns;
        // unknown lines are logged and ignored.
        run_stdin_loop(&coord_handle);

        // Shut everything down in reverse order. The coordinator joins
        // first so any in-flight stop_and_transcribe completes before
        // the sink (and its captured session / audio pump) drops; the
        // event pump then drains any tail events and exits when the
        // channel disconnects.
        coord_handle.shutdown();
        coord_thread.join();
        drop(tx); // close the supervisor side of the event channel
        let _ = pump.join();
        if let Some(handle) = hotkey_handle {
            handle.shutdown();
        }

        Ok(())
    }
}

/// Install the rdev OS hotkey listener via the existing
/// `maybe_install_rust_hotkey` plumbing. The closure is an "ignore"
/// callback because the coordinator is already wired by the session
/// sink -- the rdev manager would normally drive coordinator events
/// directly via its own bridge inside `install_hotkey`. We're using
/// `install_hotkey` here to set up the OS listener, so we MUST give
/// it a sink callback even though the coordinator is wired
/// separately. The OS listener's TrackerOutput → CoordinatorEvent
/// bridge is set up by `install_hotkey` (see `hotkey/mod.rs`); the
/// `action_sink` parameter we pass here is for a SECOND coordinator
/// that the install spawns internally.
///
/// **Architectural note**: today `install_hotkey` always spawns its
/// own coordinator and bridges the OS listener into it. The session
/// sink coordinator we spawn manually in `WorkerRunner::run` is
/// driven by stdin commands only. A future cleanup will let
/// `install_hotkey` accept an external coordinator handle so the
/// OS listener feeds the same coordinator the session sink talks to.
/// For PR 6 we accept the duplication so the rdev install path is
/// exercised on real builds without restructuring the hotkey crate.
fn install_listener(
    key_names: Vec<String>,
    mode: Mode,
    session_coord: CoordinatorHandle,
) -> Option<crate::hotkey::HotkeyHandle> {
    if key_names.is_empty() {
        eprintln!(
            "[worker-rust] no PTT chord configured (VOICEPI_KEY unset or empty); \
             rdev listener not installed -- worker is driven from stdin only"
        );
        return None;
    }
    // The bridge: forward every TrackerOutput-derived CoordinatorEvent
    // that install_hotkey's internal coordinator emits as actions into
    // OUR session-sink coordinator. install_hotkey requires an action
    // sink (FnMut(CoordinatorAction)); we translate the actions back
    // into CoordinatorEvents on the session coordinator so the session
    // sink reacts to them. This is the price of not yet letting
    // install_hotkey share a coordinator with the caller.
    use crate::hotkey::coordinator::CoordinatorAction;
    let bridge = session_coord;
    let result = crate::runtime::maybe_install_rust_hotkey(key_names, mode, move |action| {
        let event = match action {
            CoordinatorAction::StartRecording(_) => CoordinatorEvent::Press,
            CoordinatorAction::StopAndTranscribe(_) => CoordinatorEvent::Release,
            CoordinatorAction::CancelRecording(_) => CoordinatorEvent::Cancel,
        };
        bridge.send(event);
    });
    if result.is_none() {
        eprintln!(
            "[worker-rust] rdev listener install returned None; falling back to \
             stdin-only driving (rdev unavailable, feature disabled, or chord rejected)"
        );
    }
    result
}

/// Emit a one-shot `[worker-event] {"event":"status","state":"ready"}`
/// line on stderr so the supervisor knows the worker is alive BEFORE
/// any PTT press. Mirrors the Python orchestrator's startup `ready`
/// event (`runtime.py::_emit_worker_event(..., state="ready")`).
fn emit_worker_ready<W: Write>(writer: &mut W) -> Result<()> {
    let event = dictate_events::StatusEvent::new(dictate_events::WorkerStatus::Ready);
    dictate_events::emit_status(writer, &event)?;
    Ok(())
}

/// Spawn the event-pump thread that forwards [`RuntimeEvent`]s off
/// the channel to stderr/stdout. Returns the join handle so the
/// mainloop can wait for the pump to drain on shutdown.
///
/// Routing mirrors the supervisor's existing line decoders:
///
/// * [`RuntimeEvent::Worker`] -- re-serialised as a
///   `[worker-event] {...}\n` line on stderr so the parent
///   supervisor's `parse_worker_event` consumer round-trips them
///   unchanged.
/// * [`RuntimeEvent::Stderr`] / [`RuntimeEvent::Error`] -- written
///   to stderr verbatim (no `[worker-event]` prefix).
/// * [`RuntimeEvent::Stdout`] -- written to stdout verbatim.
/// * [`RuntimeEvent::Started`] / [`RuntimeEvent::Exited`] -- ignored
///   (the session sink never emits these; included for total-match
///   exhaustiveness).
fn spawn_event_pump(rx: mpsc::Receiver<RuntimeEvent>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("worker-rust-event-pump".to_owned())
        .spawn(move || {
            let mut stderr = io::stderr().lock();
            let mut stdout = io::stdout().lock();
            while let Ok(event) = rx.recv() {
                forward_event(event, &mut stderr, &mut stdout);
            }
        })
        .expect("worker-rust-event-pump spawn")
}

/// Pure-logic helper for [`spawn_event_pump`] so the routing is
/// unit-testable without a real channel / thread.
fn forward_event<E: Write, O: Write>(event: RuntimeEvent, stderr: &mut E, stdout: &mut O) {
    match event {
        RuntimeEvent::Worker(worker) => {
            // Re-serialise the worker event back into the wire form
            // the supervisor expects. The payload Value is already
            // shaped correctly (event + state + extras); we just
            // bracket it with the prefix and newline.
            let line = format!(
                "{}{}\n",
                dictate_events::WORKER_EVENT_PREFIX,
                worker.payload
            );
            let _ = stderr.write_all(line.as_bytes());
            let _ = stderr.flush();
        }
        RuntimeEvent::Stderr(line) => {
            let _ = writeln!(stderr, "{line}");
        }
        RuntimeEvent::Error(line) => {
            let _ = writeln!(stderr, "[worker-rust] error: {line}");
        }
        RuntimeEvent::Stdout(line) => {
            let _ = writeln!(stdout, "{line}");
        }
        RuntimeEvent::Started { .. } | RuntimeEvent::Exited { .. } => {
            // Not emitted by the session sink; only the supervisor's
            // own bookkeeping produces these. Worker-rust runs WITHOUT
            // a supervisor underneath it, so this branch is unreachable
            // in practice. Drop silently to keep the match exhaustive.
        }
    }
}

/// Drive the stdin command loop. Blocks the calling thread until EOF
/// or an explicit `quit` line. Each iteration trims whitespace,
/// lower-cases the command for case-insensitive matching, and
/// dispatches:
///
/// * `press`  -> [`CoordinatorEvent::Press`]
/// * `release` -> [`CoordinatorEvent::Release`]
/// * `cancel` -> [`CoordinatorEvent::Cancel`]
/// * `quit` / `exit` -> return (graceful shutdown)
/// * empty line -> ignored
/// * anything else -> stderr warning, continue
///
/// Read errors are treated as EOF: the supervisor closing stdin (or
/// the integration test ending its writes) is the canonical shutdown
/// signal on Windows where SIGTERM isn't reliable.
fn run_stdin_loop(coord: &CoordinatorHandle) {
    let stdin = io::stdin();
    let reader = BufReader::new(stdin.lock());
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!("[worker-rust] stdin read error: {err}; shutting down");
                return;
            }
        };
        let cmd = line.trim().to_ascii_lowercase();
        match dispatch_stdin_command(&cmd, coord) {
            StdinCommandOutcome::Continue => continue,
            StdinCommandOutcome::Quit => return,
        }
    }
    // EOF on stdin -> graceful shutdown (same as `quit`).
}

/// Outcome of a single stdin command dispatch. Extracted so the
/// command parser is unit-testable without a real stdin handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdinCommandOutcome {
    Continue,
    Quit,
}

/// Pure-logic dispatcher for one trimmed + lowercased stdin command.
/// Returns whether the mainloop should keep reading or shut down.
/// Side effect: sends to `coord` on press / release / cancel.
fn dispatch_stdin_command(cmd: &str, coord: &CoordinatorHandle) -> StdinCommandOutcome {
    match cmd {
        "" => StdinCommandOutcome::Continue,
        "press" => {
            coord.send(CoordinatorEvent::Press);
            StdinCommandOutcome::Continue
        }
        "release" => {
            coord.send(CoordinatorEvent::Release);
            StdinCommandOutcome::Continue
        }
        "cancel" => {
            coord.send(CoordinatorEvent::Cancel);
            StdinCommandOutcome::Continue
        }
        "quit" | "exit" => StdinCommandOutcome::Quit,
        other => {
            eprintln!(
                "[worker-rust] unknown stdin command: {other:?} (expected one of \
                 press/release/cancel/quit); ignoring"
            );
            StdinCommandOutcome::Continue
        }
    }
}
