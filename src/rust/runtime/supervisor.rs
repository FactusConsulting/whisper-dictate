//! Core [`RuntimeSupervisor`] type, its event / state ADTs, and the
//! `new` / `start` lifecycle entry points.
//!
//! Split out of `runtime.rs` in the 500-LOC modularity refactor. The
//! supervisor's remaining post-start controls (stop / restart / poll /
//! session-sink suspend / test hooks) live in
//! [`super::control`]; the feature-gated audio-bridge methods live in
//! [`super::audio_bridge`]. All three modules add `impl RuntimeSupervisor`
//! blocks that merge at compile time.

use std::process::{Child, Command, Stdio};
#[cfg(feature = "audio-in-rust")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
#[cfg(feature = "audio-in-rust")]
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::hotkey_install::{
    disable_python_hotkey, install_rust_hotkey_from_command, restart_hotkey_decision,
    RestartHotkeyDecision,
};
use super::in_process::{
    self, engine_choice_from_env, EngineChoice, InProcessInstallError, ENGINE_ENV,
};
use super::process::{
    configure_background_process, configure_piped_python_stdio, stream_lines, RuntimeStream,
};
use super::rust_session_sink;
use super::worker_command::{WorkerCommand, WORKER_EVENTS_ENV};
use super::{audio_pipeline_available, audio_pipeline_requested, audio_spawn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Stopped,
    Starting,
    Running,
}

impl RuntimeState {
    pub fn label(self) -> &'static str {
        match self {
            RuntimeState::Stopped => "Stopped",
            RuntimeState::Starting => "Starting",
            RuntimeState::Running => "Running",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeEvent {
    Started { command: String },
    Worker(WorkerEvent),
    Stdout(String),
    Stderr(String),
    Exited { code: Option<i32> },
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerEvent {
    pub event: String,
    pub state: Option<String>,
    pub payload: Value,
}

/// Optional zero-arg callback fired AFTER every event is pushed onto the
/// runtime channel. Lets the consumer (the egui UI) wake itself up so an
/// event that arrives while the window is minimized / unfocused gets
/// processed immediately instead of waiting for the next 80 ms repaint
/// tick — which, on Windows, doesn't fire when the window doesn't have
/// foreground attention. Without this, the user observed the tray icon
/// staying GREEN for a full PTT cycle after ~10 min of idle: the worker
/// emitted opening/recording/transcribing as usual, but the UI never woke
/// to process the events, so the tray-state transitions never made it to
/// the OS tray API.
pub type RepaintNotifier = std::sync::Arc<dyn Fn() + Send + Sync>;

// No `#[derive(Debug)]`: `Arc<dyn Fn() + Send + Sync>` (the repaint notifier)
// does not implement Debug. Nothing in the codebase actually formats the
// supervisor with `{:?}`, so the manual impl is dead weight; leave it off
// and `#[derive(Debug)]` can return if/when a real consumer wants it.
pub struct RuntimeSupervisor {
    pub(super) child: Option<Child>,
    pub(super) state: RuntimeState,
    pub(super) tx: Sender<RuntimeEvent>,
    pub(super) rx: Receiver<RuntimeEvent>,
    pub(super) repaint_notifier: Option<RepaintNotifier>,
    /// Active Rust→Python audio bridge, only `Some` when the worker was
    /// spawned with the Rust capture backend AND the worker has emitted
    /// `state=ready` (so the Python stdin reader is up). Wrapped in
    /// `Arc<Mutex<...>>` because a background "ready-watch" thread
    /// installs the handle here on receipt of the worker's ready event
    /// (iteration-3 review finding #1) — see `spawn_ready_watch`.
    /// `None` for the default Python sounddevice path AND for stock
    /// builds without the `audio-in-rust` cargo feature.
    #[cfg(feature = "audio-in-rust")]
    pub(super) audio_bridge: Arc<Mutex<Option<crate::audio::BridgeHandle>>>,
    /// Set by the bridge-error watcher thread when it observes a
    /// terminal `BridgeError::Io` / `BridgeError::Pipeline` event
    /// (iteration-2 review finding #3). The watcher cannot mutate
    /// `self`, so it raises this flag; the next `poll()` call sees it
    /// and tears the worker down (kills the child, drops the bridge,
    /// emits `Exited { code: None }`) so the UI flips back to Stopped
    /// instead of leaving a half-dead worker that won't transcribe.
    #[cfg(feature = "audio-in-rust")]
    pub(super) bridge_terminal: Arc<AtomicBool>,
    /// Iteration-3 review finding #1 race-guard: set by `stop()` /
    /// `poll()` teardown so the in-flight ready-watch thread (which
    /// may be mid-`pending.start()`) can detect that the supervisor
    /// no longer wants the bridge and discard the newly-opened handle
    /// instead of installing it into a stopped supervisor. Reset by
    /// `start()` on the next run.
    #[cfg(feature = "audio-in-rust")]
    pub(super) bridge_cancel: Arc<AtomicBool>,
    /// Live Rust hotkey handle, installed the first time `start()` is called
    /// when `VOICEPI_HOTKEY_BACKEND=rust` is set AND the binary was built
    /// with the `rust-hotkeys` feature. `None` when the backend is not
    /// requested or the install failed (supervisor stays on pynput then).
    ///
    /// Only installed once per process lifetime: the rdev listener thread
    /// cannot be cleanly stopped, so subsequent `restart()` calls reuse the
    /// existing handle. The coordinator retains its state machine across
    /// restarts — that is fine because the coordinator only cares about
    /// key-press events, not about which Python worker run they arrive in.
    ///
    /// P2 #346 finding 1.
    pub(super) hotkey_handle: Option<crate::hotkey::HotkeyHandle>,
}

impl Default for RuntimeSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeSupervisor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            child: None,
            state: RuntimeState::Stopped,
            tx,
            rx,
            repaint_notifier: None,
            #[cfg(feature = "audio-in-rust")]
            audio_bridge: Arc::new(Mutex::new(None)),
            #[cfg(feature = "audio-in-rust")]
            bridge_terminal: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "audio-in-rust")]
            bridge_cancel: Arc::new(AtomicBool::new(false)),
            hotkey_handle: None,
        }
    }

    /// Install a callback that fires after every runtime event is enqueued.
    /// The UI installs this on its first `update()` call so the egui context
    /// is woken whenever a worker event arrives, even when the window has no
    /// foreground attention. Idempotent — overwrites any previous notifier.
    pub fn set_repaint_notifier(&mut self, notifier: RepaintNotifier) {
        self.repaint_notifier = Some(notifier);
    }

    pub fn has_repaint_notifier(&self) -> bool {
        self.repaint_notifier.is_some()
    }

    pub fn state(&self) -> RuntimeState {
        self.state
    }

    pub fn is_running(&self) -> bool {
        // The Python-worker path pins liveness through `self.child`;
        // the in-process Phase B path never sets `self.child` (there
        // is no Python child to shepherd) but does move `self.state`
        // to `Running` after `try_install` succeeds. Callers like the
        // Settings save path use `is_running()` to gate whether a
        // config change requires a restart -- without the state
        // fallback below, changing `key` / `toggle_mode` / model
        // while Phase B is active would silently skip
        // `restart_runtime` and leave the old binding in effect
        // until a manual stop/start (Codex P2 PR #519
        // supervisor.rs:503).
        self.child.is_some() || matches!(self.state, RuntimeState::Running)
    }

    pub fn start(&mut self, command: WorkerCommand) -> Result<()> {
        self.poll();
        if self.child.is_some() {
            return Err(anyhow!("runtime is already running"));
        }
        // Reset the iteration-3 ready-watch cancel flag from any
        // previous run so a fresh start can install the freshly-built
        // bridge.
        #[cfg(feature = "audio-in-rust")]
        self.bridge_cancel.store(false, Ordering::SeqCst);

        // Audit item 5 Phase B step 1: `VOICEPI_DICTATE_ENGINE=rust`
        // routes through the in-process dispatch path (Rust supervisor
        // installs the hotkey + session sink directly, no Python
        // worker child spawned). See `docs/design/item5-phase-b-inprocess.md`.
        //
        // The branch runs BEFORE any Python-worker setup so the
        // fallback path is a clean fall-through: on any Err from
        // `attempt_in_process_start` the supervisor emits a stderr
        // line, clears `VOICEPI_DICTATE_ENGINE` on the effective
        // command's env so the spawned Python worker does not attempt
        // to re-enter the Phase A subprocess pipeline, and drops
        // through to the Python-worker spawn below.
        let mut effective_command = command;
        match engine_choice_from_env() {
            EngineChoice::Rust => {
                match self.attempt_in_process_start(&effective_command) {
                    Ok(()) => return Ok(()),
                    Err(err) => {
                        let _ = self.tx.send(RuntimeEvent::Stderr(format!(
                            "[runtime] Phase B in-process dispatch refused: {err}"
                        )));
                        // Prevent the spawned Python worker from
                        // recursively re-triggering Phase A's subprocess
                        // path: the Phase A shim reads the same env var
                        // via `vp_dictate_engine.run_rust_engine`, and
                        // without this clear a stale `=rust` inherited
                        // by the child would attempt to shell back out
                        // to `whisper-dictate dictate-run` on top of the
                        // already-failed in-process attempt.
                        clear_engine_env_for_child(&mut effective_command);
                    }
                }
            }
            EngineChoice::Unknown(raw) => {
                let _ = self.tx.send(RuntimeEvent::Stderr(format!(
                    "[runtime] Unknown {ENGINE_ENV}={raw:?} - falling back to the Python engine"
                )));
            }
            EngineChoice::Python => {}
        }

        // Phase-1 rollout (PR #341 — wiring the Rust audio pipeline):
        // If the user opted in via VOICEPI_AUDIO_BACKEND=rust AND this
        // binary was built with the `audio-in-rust` cargo feature, we
        // splice the Rust capture path into the worker's stdin and ask
        // Python to read frames from there. Default builds and the
        // env-var-unset case go through the exact same code path as
        // before — see `audio_spawn::should_use_rust_audio_backend` for
        // the precise gate. To disable in an emergency: unset the env
        // var, or rebuild without `--features audio-in-rust`.
        let use_rust_audio = audio_spawn::should_use_rust_audio_backend();
        let warn_unavailable = audio_pipeline_requested() && !audio_pipeline_available();
        if warn_unavailable {
            let _ = self.tx.send(RuntimeEvent::Stderr(format!(
                "[runtime] {}",
                audio_spawn::requested_but_unavailable_warning(),
            )));
        }

        self.state = RuntimeState::Starting;
        if use_rust_audio {
            effective_command
                .args
                .push("--audio-source=rust-stdin".to_owned());
        }

        // P2 #346 finding 1 / P1 #373: wire the Rust hotkey installer on the
        // first start() call when the user has opted in via
        // `VOICEPI_HOTKEY_BACKEND=rust`. Only installed once — the rdev
        // listener thread cannot be cleanly stopped so subsequent restart()
        // calls reuse the existing handle.
        //
        // Python stays enabled when the action sink is the logger sink:
        // the coordinator only logs `[hotkey]` lines and no IPC drives the
        // worker's actual recording lifecycle, so Python must keep
        // listening or PTT goes silent. When the user opts into the
        // session sink (`VOICEPI_DICTATE_BACKEND=rust-session`) AND that
        // install succeeds, the Rust process now drives the dictation
        // loop in-process so we MUST park the Python listener -- otherwise
        // both backends react to the same chord and produce duplicate /
        // conflicting state transitions. Codex P2 #416 runtime.rs:1427.
        if self.hotkey_handle.is_none() {
            // Legacy in-process diagnostic path
            // (`VOICEPI_DICTATE_BACKEND=rust-session` WITHOUT the Phase B
            // `VOICEPI_DICTATE_ENGINE=rust` branch, which already applies
            // this at the in-process install site): the session sink built
            // inside `install_rust_hotkey_from_command` sources its settings
            // from the process env via `*_from_env()` (whisper hints,
            // min-record floor, format commands, post-processing). Those
            // saved values live only in the worker command's env vector, so
            // apply them to the process env FIRST -- otherwise the in-process
            // session silently runs on parent-process defaults while the
            // Python child would have received the configured values
            // (Codex P2 #531).
            if rust_session_sink::dictate_backend_rust_session_requested() {
                in_process::apply_worker_command_env(&effective_command);
            }
            if let Some(handle) = install_rust_hotkey_from_command(
                &effective_command,
                self.tx.clone(),
                self.repaint_notifier.clone(),
            ) {
                if rust_session_sink::dictate_backend_rust_session_requested() {
                    disable_python_hotkey(&mut effective_command);
                }
                self.hotkey_handle = Some(handle);
            }
        } else if let Some(handle) = self.hotkey_handle.as_ref() {
            // Restart path: the handle survived from a prior start(); the
            // env-var-derived backend choice is fixed per-process so we
            // re-evaluate whether to park Python every time.
            //
            // Codex P2 #416 (round 2) runtime.rs:511 -- validate the
            // (possibly updated) key binding against the Rust backend's
            // rdev-supported list BEFORE parking Python. Without this, a
            // Settings change to a key that the Python evdev backend
            // accepts but rdev does not (eg `super_l`) would leave Python
            // disabled and the Rust hotkey unable to fire -- PTT goes
            // silent for the whole session.
            //
            // `resume()` itself only calls `manager.register()` and does
            // not re-run install-time validation, so this gate is the
            // only place restart-time key changes are sanity-checked.
            //
            // Fix 3 (#373): Resume the manager with the (possibly new) PTT
            // key names so a changed binding takes effect without
            // restarting the whole process. The coordinator mode
            // (hold-to-talk vs. toggle) is fixed at install time; mode
            // changes require an app restart.
            //
            // The branch decision is extracted to `restart_hotkey_decision`
            // so the three-way gate (no-key / unsupported / resume) is
            // unit-testable without populating a live HotkeyHandle (Codex
            // P2 PR #421 runtime.rs:530).
            match restart_hotkey_decision(
                &effective_command,
                rust_session_sink::dictate_backend_rust_session_requested(),
            ) {
                RestartHotkeyDecision::SkipNoKey => {}
                RestartHotkeyDecision::SkipUnsupported { key_names } => {
                    eprintln!(
                        "[hotkey] resume skipped: PTT keys {key_names:?} are not \
                         supported by the Rust backend; keeping Python listener \
                         engaged and skipping resume"
                    );
                }
                RestartHotkeyDecision::Resume {
                    key_names,
                    park_python,
                } => {
                    if park_python {
                        disable_python_hotkey(&mut effective_command);
                    }
                    handle.resume(key_names);
                }
            }
        }

        let display = effective_command.display();
        let mut process = Command::new(&effective_command.program);
        process
            .args(&effective_command.args)
            .current_dir(&effective_command.working_dir)
            .env(WORKER_EVENTS_ENV, "1")
            .envs(
                effective_command
                    .env
                    .iter()
                    .map(|(key, value)| (key, value)),
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if use_rust_audio {
            // Only the Rust path needs to write to the worker's stdin
            // (JSON frame events). Inherit otherwise — the Python path
            // doesn't read stdin and a piped+unused stdin can confuse
            // some libraries that probe `isatty()` on launch.
            process.stdin(Stdio::piped());
        }
        configure_piped_python_stdio(&mut process);
        configure_background_process(&mut process);
        let mut child = match process.spawn() {
            Ok(c) => c,
            Err(err) => {
                // Codex P2 #416 (round 2) runtime.rs:504 -- the
                // session sink was registered above; without this
                // cleanup PTT would still drive DictateSession after
                // a spawn failure.
                self.suspend_session_sink_on_start_failure();
                self.state = RuntimeState::Stopped;
                return Err(err.into());
            }
        };

        // Iteration-3 review finding #1: when the rust-audio backend
        // is active we need to know when the worker's stdin reader is
        // live so we can defer opening cpal until then (no pre-ready
        // frames piling up in the OS pipe buffer). The Python worker
        // emits `state=ready` on its stderr after model load and right
        // before constructing `Dictate` (which spawns the stdin reader
        // in __init__). Wire the stderr streamer to ping a one-shot
        // channel on that event; an on-the-fly ready-watch thread
        // installs the BridgeHandle once it sees the ping.
        #[cfg(feature = "audio-in-rust")]
        let ready_signal: Option<Sender<()>> = if use_rust_audio {
            let (ready_tx, ready_rx) = mpsc::channel::<()>();
            // Bridge prep up-front so a missing Silero ONNX still
            // fails-fast (synchronous Err from `start()`). The cpal
            // stream is NOT opened here — that's deferred to the
            // ready-watch thread below.
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("audio-in-rust: child stdin was not piped"))?;
            let pending = match audio_spawn::prepare_audio_bridge_for_child(stdin) {
                Ok(p) => p,
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Codex P2 #416 (round 2) runtime.rs:504 -- mirror
                    // the spawn-failure cleanup so an audio-pipeline
                    // init failure doesn't leave the session sink
                    // driving DictateSession against a dead worker.
                    self.suspend_session_sink_on_start_failure();
                    self.state = RuntimeState::Stopped;
                    return Err(anyhow!(
                        "failed to prepare Rust audio pipeline: {err}; \
                         unset VOICEPI_AUDIO_BACKEND to fall back to the \
                         Python sounddevice path"
                    ));
                }
            };
            let device = audio_spawn::resolve_audio_device_from_env(&effective_command.env);
            self.spawn_ready_watch(pending, device, ready_rx);
            Some(ready_tx)
        } else {
            None
        };
        #[cfg(not(feature = "audio-in-rust"))]
        let ready_signal: Option<Sender<()>> = None;

        if let Some(stdout) = child.stdout.take() {
            stream_lines(
                stdout,
                self.tx.clone(),
                RuntimeStream::Stdout,
                self.repaint_notifier.clone(),
                None,
            );
        }
        if let Some(stderr) = child.stderr.take() {
            stream_lines(
                stderr,
                self.tx.clone(),
                RuntimeStream::Stderr,
                self.repaint_notifier.clone(),
                ready_signal,
            );
        }

        self.state = RuntimeState::Running;
        self.child = Some(child);
        let _ = self.tx.send(RuntimeEvent::Started { command: display });
        if let Some(notifier) = self.repaint_notifier.as_ref() {
            notifier();
        }
        Ok(())
    }

    /// Audit item 5 Phase B: install the Rust dictation runtime in the
    /// UI process itself and mark the supervisor as Running without
    /// spawning a Python worker child. Any Err is a fallback signal —
    /// the caller emits a stderr line and falls through to the
    /// Python-worker path.
    ///
    /// The install is idempotent WITH the supervisor's existing
    /// `hotkey_handle` slot: on a restart() a previously-installed
    /// hotkey handle survives (the rdev / evdev listener threads
    /// cannot be cleanly stopped anyway), so we short-circuit the
    /// install and just re-enter the Running state. The very-first
    /// start() populates the slot; subsequent starts inherit the
    /// coordinator's state machine, matching the Python-worker path's
    /// P2 #346 finding 1 behaviour.
    fn attempt_in_process_start(
        &mut self,
        command: &WorkerCommand,
    ) -> std::result::Result<(), InProcessInstallError> {
        // F1 (Codex P1 PR #519 supervisor.rs:467): apply the
        // WorkerCommand's `VOICEPI_*` env vector to the process
        // environment so the in-process backends see the same view a
        // Python child would inherit through `.envs()`. Without this,
        // saved schema settings (language, initial prompt, audio
        // device, inject mode, recording thresholds, ...) that the UI
        // wrote via `worker_command()` are silently discarded when
        // the supervisor takes the Phase B path and the real backends
        // fall back to defaults. See `in_process::apply_worker_command_env`
        // for the filtering (VOICEPI_* only, RUST_INJECTOR skipped).
        in_process::apply_worker_command_env(command);

        // Design-doc risk #5: if the operator has both `ENGINE=rust`
        // AND the older `VOICEPI_DICTATE_BACKEND=rust-session` set,
        // ENGINE wins and the supervisor emits an informational line
        // naming the effective backend so the operator sees which one
        // won.
        in_process::maybe_emit_env_precedence_note(&self.tx);

        // Reuse the existing hotkey handle across restart()s: the
        // in-process runtime installs the manager + coordinator
        // threads once per process (same as the Python-worker path).
        // A fresh start() installs, subsequent restart()s just
        // re-resume the manager binding — `stop()` previously called
        // `handle.suspend()` which unregistered it, so without a
        // matching resume PTT would go silent on the second run.
        if let Some(handle) = self.hotkey_handle.as_ref() {
            let key_names = in_process::resume_key_names_from_env()
                .map_err(InProcessInstallError::ConfigLoadFailed)?;
            if key_names.is_empty() {
                return Err(InProcessInstallError::EmptyChord);
            }
            handle.resume(key_names);
        } else {
            let installation =
                in_process::try_install(self.tx.clone(), self.repaint_notifier.clone())?;
            self.stash_in_process_installation(installation);
        }

        // Emit Started + ready worker event so the UI's ready-latch
        // fires identically to the Python-worker path (design-doc
        // risk #2: status-event parity).
        self.state = RuntimeState::Running;
        let _ = self.tx.send(RuntimeEvent::Started {
            command: format!("{ENGINE_ENV}=rust (in-process)"),
        });
        in_process::emit_ready_worker_event(&self.tx);
        if let Some(notifier) = self.repaint_notifier.as_ref() {
            notifier();
        }
        Ok(())
    }

    /// Feature-gated stash — moves the installation's live handle into
    /// the supervisor's `hotkey_handle` slot AND leaks the coord-slot
    /// keepalive so the session sink's `on_processing_finished`
    /// callback survives for the process lifetime. On a stock build
    /// this is a no-op because [`try_install`] returned Err before
    /// ever constructing an `InProcessInstallation`.
    #[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
    fn stash_in_process_installation(&mut self, installation: in_process::InProcessInstallation) {
        self.hotkey_handle = Some(installation.hotkey_handle);
        // Leak the coord-slot keepalive so the sink's closure survives
        // for the process lifetime. This is intentional: the sink was
        // built inside `in_process::try_install` and captures its own
        // clone of the same `Arc<OnceLock<_>>`; dropping our clone
        // here has no visible effect on the sink, but leaking makes
        // the shape symmetric with `install_session_sink_hotkey` where
        // the slot is `Arc::clone`d into the coordinator callback.
        std::mem::forget(installation.coord_slot_keepalive);
    }

    #[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
    #[allow(dead_code)]
    fn stash_in_process_installation(&mut self, _installation: in_process::InProcessInstallation) {
        // Unreachable: `try_install` returned FeaturesMissing before
        // constructing an installation on stock builds. Kept so the
        // supervisor's `start()` type-checks under every feature
        // configuration without a `#[cfg]` at every call site.
    }
}

/// Strip `VOICEPI_DICTATE_ENGINE` from the effective worker command's
/// env vector so a fallback-to-Python spawn does not re-enter the Phase
/// A subprocess pipeline (the Python worker would see `=rust` and
/// shell out to `whisper-dictate dictate-run` on top of our
/// already-failed in-process attempt). Idempotent — the entry may be
/// absent because the UI process propagates its own env by default.
fn clear_engine_env_for_child(command: &mut WorkerCommand) {
    command.env.retain(|(key, _)| key != ENGINE_ENV);
    command
        .env
        .push((ENGINE_ENV.to_owned(), in_process::ENGINE_PYTHON.to_owned()));
}

impl Drop for RuntimeSupervisor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
