//! Wire the hotkey coordinator's
//! [`crate::hotkey::coordinator::CoordinatorAction`] sink into a
//! [`crate::dictate::DictateSession`] so PTT press/release actually
//! drives `session.start()` / `stop_and_transcribe()` / `cancel(epoch)`
//! instead of merely logging.
//!
//! Wave 5 PR 4 of #348. **Opt-in only** behind
//! `VOICEPI_DICTATE_BACKEND=rust-session` -- production keeps logging
//! actions (and the Python orchestrator keeps owning the live PTT loop)
//! until PR 6 ships the full Rust worker and flips the default.
//!
//! # Slicing
//!
//! - PR 4 (this module): coordinator → session wire-up + stub backends
//!   so the end-to-end flow is observable without `whisper-rs-local` or
//!   the OS injector. The stub `TranscribeBackend` always returns an
//!   empty result with a `"rust-session-pr4-stub"` gate so the session
//!   takes the `no_text` branch and emits the matching worker event;
//!   the stub `InjectBackend` is a no-op for the same reason.
//! - PR 5 (follow-up): swap the stubs for `LocalWhisper` +
//!   the existing injection dispatcher; wire `audio_route` (PR 3 #415)
//!   into [`crate::dictate::DictateSession::push_frame`].
//! - PR 6 (follow-up): flip the default so the Rust supervisor owns the
//!   PTT loop end-to-end (no env-var gate needed).
//!
//! # Why a tiny module instead of inline in `runtime.rs`
//!
//! `runtime.rs` is already past the 500-LOC guideline; the new wiring
//! lives here so the guideline is respected (per AGENTS.md "Modularity").
//! The integration test exercising the full coordinator → sink →
//! session → worker-events loop lives in the `#[cfg(test)] mod tests`
//! block at the bottom of this file so it sits next to the code under
//! test.

use std::io::Write;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use crate::dictate::{
    DictateSession, InjectBackend, InjectError, SessionConfig, TranscribeBackend, TranscribeError,
    TranscribeResult,
};
use crate::hotkey::coordinator::{CoordinatorAction, CoordinatorEvent, CoordinatorHandle};
use crate::runtime::{RuntimeEvent, WorkerEvent};

/// Env-var name. Matches the existing `VOICEPI_DICTATE_BACKEND` env var
/// the Python wrapper reads -- the `rust-session` value is the new
/// opt-in for the in-process Rust session sink (alongside the existing
/// `rust` value the Python wrapper interprets as "shell out to
/// `dictate-ops`").
pub(crate) const DICTATE_BACKEND_ENV: &str = "VOICEPI_DICTATE_BACKEND";

/// Value that enables the in-process Rust session sink wiring.
pub(crate) const DICTATE_BACKEND_RUST_SESSION: &str = "rust-session";

/// Prefix every `[worker-event] {…}` line carries. Mirrors the
/// `WORKER_EVENT_PREFIX` const in `runtime.rs`; kept local so this
/// module compiles standalone (and so a future refactor of the runtime
/// constant does not force a sibling-module rename). `pub(super)` so
/// the sibling [`super::rust_session_sink_tests`] module can spell the
/// prefix literally in its assertions.
pub(super) const WORKER_EVENT_PREFIX: &str = "[worker-event] ";

/// True when the user opted in to the Rust-session sink wiring via env
/// var. Pure helper (no side effects) so the gate is unit-testable
/// without spawning a coordinator. Returns false for unset / empty /
/// any non-`rust-session` value.
pub(crate) fn dictate_backend_rust_session_requested() -> bool {
    std::env::var(DICTATE_BACKEND_ENV)
        .map(|v| v.trim().eq_ignore_ascii_case(DICTATE_BACKEND_RUST_SESSION))
        .unwrap_or(false)
}

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
pub(crate) fn build_session_action_sink<F>(
    session: Arc<Mutex<StubSession>>,
    tx: Sender<RuntimeEvent>,
    on_processing_finished: F,
) -> impl FnMut(CoordinatorAction) + Send + 'static
where
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
        let mut forwarder = EventForwarder::new(&tx);
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
/// Used only from the supervisor; tests construct the sink directly via
/// [`build_session_action_sink`] so they can plug a recording callback
/// in place of the OnceLock dance.
pub(crate) fn build_production_sink(
    tx: Sender<RuntimeEvent>,
) -> (
    impl FnMut(CoordinatorAction) + Send + 'static,
    Arc<OnceLock<CoordinatorHandle>>,
) {
    let coord_slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());
    let coord_slot_for_signal = Arc::clone(&coord_slot);
    let session = make_session();
    let sink = build_session_action_sink(session, tx, move |id| {
        if let Some(handle) = coord_slot_for_signal.get() {
            handle.send(CoordinatorEvent::ProcessingFinished(id));
        }
    });
    (sink, coord_slot)
}

// ── event forwarder ──────────────────────────────────────────────────────────

/// `Write` adapter that buffers bytes until a newline, then ships each
/// complete line as a [`RuntimeEvent`]. `[worker-event] {…}` lines are
/// parsed into [`RuntimeEvent::Worker`]; anything else (or a malformed
/// payload) lands as [`RuntimeEvent::Stderr`] so the supervisor's log
/// card still picks it up. `pub(super)` so the sibling
/// [`super::rust_session_sink_tests`] module can construct one
/// directly and assert its framing without going through the sink.
pub(super) struct EventForwarder<'a> {
    tx: &'a Sender<RuntimeEvent>,
    buf: Vec<u8>,
}

impl<'a> EventForwarder<'a> {
    pub(super) fn new(tx: &'a Sender<RuntimeEvent>) -> Self {
        Self {
            tx,
            buf: Vec::new(),
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
            let _ = self.tx.send(event);
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
