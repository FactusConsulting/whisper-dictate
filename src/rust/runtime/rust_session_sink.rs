//! Wire the hotkey coordinator's
//! [`crate::hotkey::coordinator::CoordinatorAction`] sink into a
//! [`crate::dictate::DictateSession`] so PTT press/release actually
//! drives `session.start()` / `stop_and_transcribe()` / `cancel(epoch)`
//! instead of merely logging.
//!
//! After PR 7 the sink is the production dictation path invoked from
//! [`super::worker_rust::WorkerRunner::run`] via
//! [`build_production_sink`]; on the `python-legacy` fallback path the
//! supervisor's own hotkey install still routes to the session sink
//! when a build carries `--features whisper-rs-local,rust-injection`.
//!
//! The env-var gate helpers that decide the fallback path live in the
//! sibling [`super::rust_session_dictate_env`] module; this file
//! re-exports them so callers keep writing
//! `rust_session_sink::dictate_backend_rust_session_requested` /
//! `_python_legacy_requested` unchanged (PR #441 review round 2 split
//! for the AGENTS.md ~500-LOC-per-file modularity guideline). Sibling
//! test files: `rust_session_sink_tests`, `_e2e_tests`, `_coverage_tests`.

use std::io::Write;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use crate::dictate::{
    DictateSession, InjectBackend, InjectError, SessionConfig, TranscribeBackend, TranscribeError,
    TranscribeResult,
};
use crate::hotkey::coordinator::{CoordinatorAction, CoordinatorEvent, CoordinatorHandle};
use crate::runtime::{RepaintNotifier, RuntimeEvent, WorkerEvent};

// Re-export the env-var gate helpers so the public API of this module
// stays byte-identical to the pre-split shape (PR #441 review round 2).
// Callers keep writing `rust_session_sink::dictate_backend_..._requested()`
// exactly as before; the implementation lives in the sibling module.
//
// Wave 8 Part 2 dropped `dictate_backend_rust_session_requested` -- the
// supervisor now unconditionally routes through the session sink on a
// full-feature build, so the "opt in via VOICEPI_DICTATE_BACKEND=rust-session"
// branch has no consumer.
pub(crate) use super::rust_session_dictate_env::dictate_backend_python_legacy_requested_from;

#[cfg(test)]
pub(crate) use super::rust_session_dictate_env::dictate_backend_python_legacy_requested;

// Constants are only referenced from `#[cfg(test)]` sibling test
// modules; a non-test lib build has no consumer for the re-export and
// clippy `-D unused-imports` fires. Gate the constant re-export on
// test only.
#[cfg(test)]
pub(crate) use super::rust_session_dictate_env::{
    DICTATE_BACKEND_ENV, DICTATE_BACKEND_PYTHON_LEGACY,
};

/// Prefix every `[worker-event] {…}` line carries. Mirrors the
/// `WORKER_EVENT_PREFIX` const in `runtime.rs`; kept local so this
/// module compiles standalone (and so a future refactor of the runtime
/// constant does not force a sibling-module rename). `pub(super)` so
/// the sibling [`super::rust_session_sink_tests`] module can spell the
/// prefix literally in its assertions.
pub(super) const WORKER_EVENT_PREFIX: &str = "[worker-event] ";

// ── stub backends ────────────────────────────────────────────────────────────

/// **PR 5 will replace this** with the real `LocalWhisper` backend.
/// Returns an empty-text result with a stub gate string so the session
/// takes the `no_text` branch and emits the matching worker event,
/// proving the wire-up without pulling the heavy `whisper-rs-local`
/// feature into the dependency graph. The gate string passes through
/// [`crate::dictate::session::normalize_gate_reason`] and lands as
/// `reason="empty"` on the emitted event (the normalizer matches on
/// `"too quiet"` / `"no speech"` substrings only).
#[derive(Debug, Default)]
pub(crate) struct StubTranscribe;

/// Gate string the stub backend uses so a reader can grep for it in the
/// worker-event stream and confirm the Rust-session path ran. Exposed
/// so the test can spell the expected value literally.
pub(crate) const STUB_GATE_STRING: &str = "rust-session-pr4-stub";

impl TranscribeBackend for StubTranscribe {
    fn transcribe(
        &self,
        _pcm: &[f32],
        _sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        Ok(TranscribeResult {
            text: String::new(),
            gate: Some(STUB_GATE_STRING.to_owned()),
            ..Default::default()
        })
    }
}

/// **PR 5 will replace this** with the existing injection dispatcher.
/// No-op for PR 4 so a stub transcription that did produce text would
/// still flow without touching the user's keyboard. The stub
/// `TranscribeBackend` above produces empty text, so this is dead-code
/// in the default path; kept implemented so the trait bound resolves
/// and the session compiles.
#[derive(Debug, Default)]
pub(crate) struct StubInject;

impl InjectBackend for StubInject {
    fn inject(&self, _text: &str) -> Result<(), InjectError> {
        Ok(())
    }
}

/// Convenience alias for the session type used by this module.
pub(crate) type StubSession = DictateSession<StubTranscribe, StubInject>;

/// Build a fresh stub-backed session wrapped in
/// `Arc<Mutex<…>>`. Exposed so the integration test can hold a clone
/// for direct `push_frame` access (the action sink only owns its own
/// clone and never exposes the session to the caller).
pub(crate) fn make_session() -> Arc<Mutex<StubSession>> {
    Arc::new(Mutex::new(DictateSession::new(
        StubTranscribe,
        StubInject,
        SessionConfig::default(),
    )))
}

// ── sink factory ─────────────────────────────────────────────────────────────

/// Build the action sink that drives `session` from
/// [`CoordinatorAction`]s and signals `ProcessingFinished` back through
/// `on_processing_finished` after a stop completes.
///
/// `on_processing_finished` is invoked from the coordinator thread
/// after [`DictateSession::stop_and_transcribe`] returns (success or
/// error). Production wires it to `coord_handle.send(ProcessingFinished(id))`
/// via a shared `OnceLock<CoordinatorHandle>` populated after
/// `install_hotkey` returns; tests substitute a closure that records
/// the id for assertion.
///
/// Each worker-event line the session writes is forwarded onto `tx`:
/// `[worker-event] {…}` lines are parsed into [`RuntimeEvent::Worker`]
/// (so consumers like the egui log card key off the same variant they
/// see for the Python worker today); anything else lands as
/// [`RuntimeEvent::Stderr`].
pub(crate) fn build_session_action_sink<T, I, F>(
    session: Arc<Mutex<DictateSession<T, I>>>,
    tx: Sender<RuntimeEvent>,
    on_processing_finished: F,
    repaint_notifier: Option<RepaintNotifier>,
) -> impl FnMut(CoordinatorAction) + Send + 'static
where
    T: TranscribeBackend + Send + 'static,
    I: InjectBackend + Send + 'static,
    F: Fn(u64) + Send + Sync + 'static,
{
    let session_for_sink = Arc::clone(&session);
    move |action: CoordinatorAction| {
        // `lock()` poisoning only happens if a previous sink invocation
        // panicked while holding the lock. In that case the session is
        // in an indeterminate state; recover the inner so we at least
        // attempt a graceful shutdown / cancel rather than wedge the
        // coordinator (which would silently drop every subsequent PTT
        // press). Subsequent calls return a fresh `MutexGuard` (the
        // poison flag stays set, but `into_inner` doesn't clear it).
        let mut session_guard = session_for_sink
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut forwarder = EventForwarder::new(&tx, repaint_notifier.as_ref());
        match action {
            CoordinatorAction::StartRecording(id) => {
                if let Err(err) = session_guard.start(&mut forwarder) {
                    let _ = tx.send(RuntimeEvent::Error(format!(
                        "[rust-session] start failed (coord id={id}): {err}"
                    )));
                }
                // No `processing_finished` here -- the coordinator is in
                // `Stage::Recording` and only the matching stop /
                // cancel transitions it out.
            }
            CoordinatorAction::StopAndTranscribe(id) => {
                let outcome = session_guard.stop_and_transcribe(&mut forwarder);
                // Drop the guard BEFORE the callback so a callback that
                // happens to re-enter the sink (e.g. test bouncing
                // ProcessingFinished + next Press immediately) does not
                // deadlock on the same mutex.
                drop(session_guard);
                drop(forwarder);
                if let Err(err) = outcome {
                    let _ = tx.send(RuntimeEvent::Error(format!(
                        "[rust-session] stop failed (coord id={id}): {err}"
                    )));
                }
                // Unblock the coordinator's `Stage::Processing` guard so
                // the next press is acted on. Always called -- even on
                // error -- to mirror the Python `_processing_finished`
                // callback's `finally:` semantics.
                on_processing_finished(id);
            }
            CoordinatorAction::CancelRecording(id) => {
                // The coordinator id IS the session epoch (the session
                // returns its epoch from `start()` and `start_recording`
                // bumps a parallel counter on the coordinator side --
                // both start at 1 and tick together as long as the sink
                // mirrors every `StartRecording` into a `start()` call).
                // A stale cancel from a previous cycle is no-op'd by the
                // session's own epoch guard (`cancel()` ignores
                // `requested_epoch != active_id`), so passing the coord
                // id straight through is safe even if the two ever drift.
                if let Err(err) = session_guard.cancel(id, &mut forwarder) {
                    let _ = tx.send(RuntimeEvent::Error(format!(
                        "[rust-session] cancel failed (coord id={id}): {err}"
                    )));
                }
                // Cancel does NOT enter `Stage::Processing` -- the
                // coordinator drops straight back to Idle on its own --
                // so no `processing_finished` signal needed.
            }
        }
    }
}

/// Combined builder for the production wiring: returns the action sink
/// AND the [`OnceLock`] the supervisor populates from the live
/// [`crate::hotkey::HotkeyHandle::coordinator_handle`] after install.
///
/// Also enables the `VOICEPI_WORKER_EVENTS` env-gate on the current
/// process so the session's wire emitter (which mirrors Python's
/// `_emit_worker_event` and short-circuits when the var is falsy) does
/// not silently drop every event from the in-process session. The
/// supervisor already sets the var on the Python child via the worker
/// command's env; the in-process session reads the Rust supervisor's
/// own env, so without this set the event stream would be empty for
/// users who haven't manually exported the var. Codex P2 #416
/// rust_session_sink.rs:179.
///
/// `repaint_notifier` is the UI's wake-up callback (the same one
/// `RuntimeSupervisor::stream_lines` runs after every event it
/// enqueues). Threading it here so the in-process session's events
/// don't sit in the channel until some unrelated repaint -- on
/// Windows with the window minimised, the egui tick doesn't fire
/// without an explicit nudge. Codex P2 #416 rust_session_sink.rs:289.
///
/// Used only from the supervisor; tests construct the sink directly via
/// [`build_session_action_sink`] so they can plug a recording callback
/// in place of the OnceLock dance.
/// Boxed action-sink closure handed back from [`build_production_sink`].
/// Aliased so clippy's `type_complexity` lint stays quiet (the tuple
/// return type otherwise breaches the threshold). The `Box<dyn …>`
/// indirection is needed because PR 5 chooses between the stub-backed
/// session (always available) and the real-backed session (gated on
/// `all(feature = "whisper-rs-local", feature = "rust-injection")`) at
/// runtime — the two underlying closures have different capture types
/// and so cannot share an `impl FnMut` return.
pub(crate) type CoordinatorActionSink = Box<dyn FnMut(CoordinatorAction) + Send + 'static>;

/// Type-alias for the "processing finished" post-transcription
/// fan-out slot. Populated by the caller AFTER the session
/// coordinator + hotkey coordinator are both live. When a session's
/// `stop_and_transcribe` completes, the sink fires every registered
/// handle in this vec with `CoordinatorEvent::ProcessingFinished(id)`.
///
/// Codex #453 P1 (runtime.rs:718): the worker-rust subprocess spawns
/// TWO coordinators -- the session coord (drives DictateSession) AND
/// the internal `install_hotkey` coord (owns rdev state). If
/// processing_finished only reaches the session coord, the hotkey
/// coord stays in `Stage::Processing` forever and every subsequent
/// PTT press is dropped. Route the fan-out through both.
pub(crate) type ProcessingFinishedFanout = Arc<Mutex<Vec<CoordinatorHandle>>>;

/// Fresh, empty fan-out slot. Callers populate before the first
/// StopAndTranscribe fires.
pub(crate) fn new_processing_finished_fanout() -> ProcessingFinishedFanout {
    Arc::new(Mutex::new(Vec::new()))
}

/// Bundle returned from [`try_build_real_production_sink`]. Adds a
/// secondary `hotkey_coord_slot` to the historical
/// `(sink, coord_slot)` pair so the caller can wire the internal
/// hotkey coordinator into the processing_finished fan-out AFTER
/// `install_hotkey` returns a handle.
pub(crate) struct StrictProductionSink {
    pub sink: CoordinatorActionSink,
    pub coord_slot: Arc<OnceLock<CoordinatorHandle>>,
    pub processing_finished_fanout: ProcessingFinishedFanout,
}

/// Codex #453 P2 (runtime.rs:662): strict variant of
/// [`build_production_sink`] that FAILS instead of falling through
/// to the PR-4 stub session on a real-backend init error. Used by
/// `worker-rust`'s child mainloop where Wave 8 Part 2 made
/// delegation mandatory -- silently downgrading to the stub would
/// leave the worker looking alive (state=ready, hotkey installed)
/// while every utterance produces empty text.
///
/// **Codex #453 P2 (rust_session_sink.rs:291) round 2**: the earlier
/// iteration probed via `make_real_session` and then delegated to
/// `build_production_sink` -- which itself called `make_real_session`
/// AGAIN and re-applied the permissive stub fallback. If the second
/// init failed while the probe succeeded, the strict path returned
/// Ok wrapping a stub. Route the sink build around the SAME `deps`
/// bundle the probe produced so a single `make_real_session` call
/// underpins both the probe result AND the shipped sink.
///
/// On a stock build (either feature missing) this returns Err right
/// away.
pub(crate) fn try_build_real_production_sink(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) -> Result<StrictProductionSink, anyhow::Error> {
    #[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
    {
        let deps = super::rust_session_real_backends::make_real_session(
            tx.clone(),
            repaint_notifier.clone(),
        )
        .map_err(|err| {
            anyhow::anyhow!(
                "real-backend session init failed ({err}); worker refuses to start with stub \
                 backends -- fix VOICEPI_WHISPER_MODEL_PATH, download a model via \
                 `whisper-dictate models download tiny.en`, or check audio/VAD/cpal init"
            )
        })?;

        let coord_slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());
        let fanout = new_processing_finished_fanout();

        let coord_slot_for_signal = Arc::clone(&coord_slot);
        let fanout_for_signal = Arc::clone(&fanout);
        let inner = build_session_action_sink(
            Arc::clone(&deps.session),
            tx,
            move |id| {
                // Session coord (owner of Stage transitions) first.
                if let Some(handle) = coord_slot_for_signal.get() {
                    handle.send(CoordinatorEvent::ProcessingFinished(id));
                }
                // Codex #453 P1 (runtime.rs:718) fan-out: every
                // secondary coordinator registered by the caller
                // (worker-rust's internal `install_hotkey` coord) also
                // hears the completion so its Stage::Processing latch
                // releases and the NEXT PTT press is not dropped.
                if let Ok(handles) = fanout_for_signal.lock() {
                    for handle in handles.iter() {
                        handle.send(CoordinatorEvent::ProcessingFinished(id));
                    }
                }
            },
            repaint_notifier,
        );

        // Move deps into a wrapper closure so the audio pump + session
        // Arc stay alive for the sink's lifetime. (Same shape as
        // `build_production_sink`'s happy path.)
        let mut inner = inner;
        let _deps_keepalive = deps;
        let owning_sink = move |action: CoordinatorAction| {
            let _keepalive = &_deps_keepalive;
            inner(action);
        };
        Ok(StrictProductionSink {
            sink: Box::new(owning_sink),
            coord_slot,
            processing_finished_fanout: fanout,
        })
    }
    #[cfg(not(all(feature = "whisper-rs-local", feature = "rust-injection")))]
    {
        let _ = (tx, repaint_notifier);
        Err(anyhow::anyhow!(
            "worker-rust cannot build a real session sink without the \
             `whisper-rs-local + rust-injection` cargo features; this branch is \
             unreachable in production because `handle_worker_rust` refuses to \
             run on stock builds"
        ))
    }
}

pub(crate) fn build_production_sink(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) -> (CoordinatorActionSink, Arc<OnceLock<CoordinatorHandle>>) {
    // Enable the worker-event gate once at sink construction. Setting
    // is idempotent and the supervisor calls this exactly once per
    // process lifetime (first `start()` with VOICEPI_DICTATE_BACKEND
    // =rust-session set), so there is no env-mutation hazard despite
    // the lack of `crate::test_env_lock::ENV_LOCK` here -- the
    // supervisor is single-threaded with respect to its own setup.
    std::env::set_var(crate::dictate::events::WORKER_EVENTS_ENV, "1");

    let coord_slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());

    // Wave 5 PR 5: when the binary was built with both `whisper-rs-local`
    // (real Whisper inference) and `rust-injection` (real OS injection)
    // the production sink uses the REAL backend trait impls instead of
    // the PR 4 stubs. On any feature missing OR a model-resolution
    // failure at construction time we fall back to the stubs so the
    // wire-up still installs (and the supervisor surfaces a stderr
    // event so the user notices the degraded mode). See
    // [`super::rust_session_real_backends`] for the constructor.
    #[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
    {
        // Wave 5 PR 5 round 2 (Codex P1 #423 finding 1): pass the
        // runtime tx + repaint notifier down to the real-backend
        // constructor so the audio pump it spawns can surface device
        // errors on the same channel the rest of the supervisor uses
        // and wake the egui UI on minimised-window installs.
        match super::rust_session_real_backends::make_real_session(
            tx.clone(),
            repaint_notifier.clone(),
        ) {
            Ok(deps) => {
                let coord_slot_for_signal = Arc::clone(&coord_slot);
                let inner = build_session_action_sink(
                    Arc::clone(&deps.session),
                    tx,
                    move |id| {
                        if let Some(handle) = coord_slot_for_signal.get() {
                            handle.send(CoordinatorEvent::ProcessingFinished(id));
                        }
                    },
                    repaint_notifier,
                );
                // Move the deps bundle into a wrapper closure so the
                // audio pump (and the session Arc) stay alive for
                // the sink's lifetime. The wrapper delegates to the
                // inner sink -- it exists purely to own the deps.
                // Without this the audio pump would be dropped right
                // after construction and no frames would reach
                // push_frame. Codex P1 #423 finding 1.
                let mut inner = inner;
                let _deps_keepalive = deps;
                let owning_sink = move |action: CoordinatorAction| {
                    let _keepalive = &_deps_keepalive;
                    inner(action);
                };
                return (Box::new(owning_sink), coord_slot);
            }
            Err(err) => {
                let _ = tx.send(RuntimeEvent::Stderr(format!(
                    "[rust-session] real backend init failed ({err}); \
                     falling back to PR 4 stub backends so the wire-up still \
                     installs. Set VOICEPI_WHISPER_MODEL_PATH or download a \
                     model via `whisper-dictate models download tiny.en` to \
                     enable real transcription."
                )));
                // fall through to the stub builder below
            }
        }
    }

    let coord_slot_for_signal = Arc::clone(&coord_slot);
    let session = make_session();
    let sink = build_session_action_sink(
        session,
        tx,
        move |id| {
            if let Some(handle) = coord_slot_for_signal.get() {
                handle.send(CoordinatorEvent::ProcessingFinished(id));
            }
        },
        repaint_notifier,
    );
    (Box::new(sink), coord_slot)
}

// ── event forwarder ──────────────────────────────────────────────────────────

/// `Write` adapter that buffers bytes until a newline, then ships each
/// complete line as a [`RuntimeEvent`]. `[worker-event] {…}` lines are
/// parsed into [`RuntimeEvent::Worker`]; anything else (or a malformed
/// payload) lands as [`RuntimeEvent::Stderr`] so the supervisor's log
/// card still picks it up. `pub(super)` so the sibling
/// [`super::rust_session_sink_tests`] module can construct one
/// directly and assert its framing without going through the sink.
///
/// Optionally carries a [`RepaintNotifier`] -- when set, the notifier
/// is invoked AFTER each event is enqueued onto `tx` so the egui UI
/// wakes up to process it. Without this the session's events can sit
/// in the channel until some unrelated repaint (the Windows
/// minimised-window pattern documented in
/// `RuntimeSupervisor::repaint_notifier`). Codex P2 #416
/// rust_session_sink.rs:289.
pub(super) struct EventForwarder<'a> {
    tx: &'a Sender<RuntimeEvent>,
    buf: Vec<u8>,
    repaint_notifier: Option<&'a RepaintNotifier>,
    /// Codex P2 (runtime.rs:2074, PR #440) — generation snapshot
    /// captured at construction so the stale-observer guard in
    /// `output_mute::session::observe_worker_state` can no-op a
    /// forwarder from a previous session if the controller was
    /// swapped mid-flight.
    mute_generation: u64,
}

impl<'a> EventForwarder<'a> {
    pub(super) fn new(
        tx: &'a Sender<RuntimeEvent>,
        repaint_notifier: Option<&'a RepaintNotifier>,
    ) -> Self {
        Self {
            tx,
            buf: Vec::new(),
            repaint_notifier,
            mute_generation: crate::output_mute::session::current_generation(),
        }
    }

    fn flush_complete_lines(&mut self) {
        while let Some(nl) = self.buf.iter().position(|b| *b == b'\n') {
            // `drain(..=nl)` includes the `\n`; we strip it for the
            // event payload but keep it in the drain range so the
            // buffer is consumed.
            let line_bytes: Vec<u8> = self.buf.drain(..=nl).collect();
            let without_nl = &line_bytes[..line_bytes.len() - 1];
            // Lossy conversion: the session's wire emitter ASCII-escapes
            // every payload byte >= 0x80 (see `wire::write_ascii_escaped`),
            // so the input is always valid UTF-8. `from_utf8_lossy` keeps
            // the forwarder defensive against a future emitter change.
            let line = String::from_utf8_lossy(without_nl).into_owned();
            let event = parse_or_stderr(line);
            // Codex P2 (runtime.rs:1992, PR #440) — in the
            // `VOICEPI_DICTATE_BACKEND=rust-session` path, worker
            // events flow through the in-process EventForwarder rather
            // than the supervisor's `stream_lines` subprocess reader.
            // The auto-mute observer used to be hooked only into the
            // latter, so the mute feature silently did nothing under
            // the rust-session backend. Fanning every state transition
            // into the observer here restores parity — cheap no-op
            // when no controller is installed.
            if let RuntimeEvent::Worker(worker) = &event {
                // Codex P2 (runtime.rs:2074 + state.rs:158, PR #440) —
                // pass the captured generation (stale-reader guard) and
                // surface any backend failure through a Stderr event so
                // the user sees when the mute silently didn't happen.
                if let Some(err) = crate::output_mute::session::observe_worker_state(
                    worker.state.as_deref(),
                    self.mute_generation,
                ) {
                    let _ = self.tx.send(RuntimeEvent::Stderr(format!(
                        "[output-mute] backend failure while observing state {state:?}: {err}",
                        state = worker.state.as_deref().unwrap_or(""),
                    )));
                }
            }
            let _ = self.tx.send(event);
            if let Some(notifier) = self.repaint_notifier {
                notifier();
            }
        }
    }
}

impl<'a> Write for EventForwarder<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        self.flush_complete_lines();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> Drop for EventForwarder<'a> {
    fn drop(&mut self) {
        // A partial line (no trailing newline) would normally indicate
        // a wire-emitter bug -- the session always emits whole lines --
        // but we still surface it as Stderr so the partial output is
        // not silently lost.
        if !self.buf.is_empty() {
            let trailing = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&trailing).into_owned();
            let _ = self.tx.send(RuntimeEvent::Stderr(line));
            if let Some(notifier) = self.repaint_notifier {
                notifier();
            }
        }
    }
}

/// Parse one already-newline-stripped line into the matching
/// [`RuntimeEvent`]. `pub(super)` so the sibling
/// [`super::rust_session_sink_tests`] module can pin the routing
/// without sending through the sink.
pub(super) fn parse_or_stderr(line: String) -> RuntimeEvent {
    let Some(raw) = line.strip_prefix(WORKER_EVENT_PREFIX) else {
        return RuntimeEvent::Stderr(line);
    };
    let Ok(payload) = serde_json::from_str::<Value>(raw) else {
        return RuntimeEvent::Stderr(line);
    };
    let Some(event_name) = payload.get("event").and_then(|v| v.as_str()) else {
        return RuntimeEvent::Stderr(line);
    };
    let event = event_name.to_owned();
    let state = payload
        .get("state")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    RuntimeEvent::Worker(WorkerEvent {
        event,
        state,
        payload,
    })
}
