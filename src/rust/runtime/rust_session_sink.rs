#![allow(dead_code)]

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
//!   the existing injection dispatcher and feed native capture frames into
//!   [`crate::dictate::DictateSession::push_frame`].
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
use crate::runtime::{RepaintNotifier, RuntimeEvent, WorkerEvent};

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
    Arc::new(Mutex::new(
        DictateSession::new(StubTranscribe, StubInject, SessionConfig::default())
            .with_worker_events_enabled(),
    ))
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
    build_session_action_sink_with_live_overrides(
        session,
        tx,
        on_processing_finished,
        repaint_notifier,
        super::live_settings::LiveEnvOverrides::default(),
        false,
    )
}

pub(super) fn build_session_action_sink_with_live_overrides<T, I, F>(
    session: Arc<Mutex<DictateSession<T, I>>>,
    tx: Sender<RuntimeEvent>,
    on_processing_finished: F,
    repaint_notifier: Option<RepaintNotifier>,
    live_env_overrides: super::live_settings::LiveEnvOverrides,
    runtime_boundaries: bool,
) -> impl FnMut(CoordinatorAction) + Send + 'static
where
    T: TranscribeBackend + Send + 'static,
    I: InjectBackend + Send + 'static,
    F: Fn(u64) + Send + Sync + 'static,
{
    let session_for_sink = Arc::clone(&session);
    let mut release_tail = std::time::Duration::from_millis(200);
    move |action: CoordinatorAction| {
        if crate::diag::debug_enabled() {
            crate::diag::log!("[dispatch] coordinator_action={action:?}");
        }
        match action {
            CoordinatorAction::StartRecording(id) => {
                let mut session_guard = session_for_sink
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if runtime_boundaries {
                    match super::live_settings::reload(&mut session_guard, &live_env_overrides) {
                        Ok(tail) => release_tail = tail,
                        Err(err) => {
                            report_live_reload_failure(&tx, repaint_notifier.as_ref(), &err)
                        }
                    }
                }
                let mut forwarder = EventForwarder::new(&tx, repaint_notifier.as_ref());
                let start_result = session_guard.start(&mut forwarder);
                match &start_result {
                    Ok(_) => {
                        if crate::diag::debug_enabled() {
                            crate::diag::log!("[dispatch] session_start emitted coord_id={id}");
                        }
                    }
                    Err(err) => {
                        if crate::diag::debug_enabled() {
                            crate::diag::log!(
                                "[dispatch] session_start refused coord_id={id} reason={err}"
                            );
                        }
                        let _ = tx.send(RuntimeEvent::Error(format!(
                            "[rust-session] start failed (coord id={id}): {err}"
                        )));
                    }
                }
            }
            CoordinatorAction::StopAndTranscribe(id) => {
                // Python reloads at the top of `_stop_and_transcribe`, then
                // keeps capture open for release_tail_ms. Refresh while holding
                // the session lock, release it so the audio pump can append tail
                // frames, and reacquire only when the commit begins.
                if runtime_boundaries {
                    let reload_result = {
                        let mut session_guard = session_for_sink
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        super::live_settings::reload(&mut session_guard, &live_env_overrides)
                    };
                    match reload_result {
                        Ok(tail) => release_tail = tail,
                        Err(err) => {
                            report_live_reload_failure(&tx, repaint_notifier.as_ref(), &err)
                        }
                    }
                    if !release_tail.is_zero() {
                        std::thread::sleep(release_tail);
                    }
                }
                let mut session_guard = session_for_sink
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                let mut forwarder = EventForwarder::new(&tx, repaint_notifier.as_ref());
                let outcome = session_guard.stop_and_transcribe(&mut forwarder);
                drop(session_guard);
                drop(forwarder);
                if let Err(err) = &outcome {
                    if crate::diag::debug_enabled() {
                        crate::diag::log!(
                            "[dispatch] session_stop refused coord_id={id} reason={err}"
                        );
                    }
                    let _ = tx.send(RuntimeEvent::Error(format!(
                        "[rust-session] stop failed (coord id={id}): {err}"
                    )));
                } else if crate::diag::debug_enabled() {
                    crate::diag::log!("[dispatch] session_stop emitted coord_id={id}");
                }
                on_processing_finished(id);
                if crate::diag::debug_enabled() {
                    crate::diag::log!("[dispatch] processing_finished_signalled coord_id={id}");
                }
            }
            CoordinatorAction::CancelRecording(id) => {
                let mut session_guard = session_for_sink
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                let mut forwarder = EventForwarder::new(&tx, repaint_notifier.as_ref());
                let cancel_result = session_guard.cancel(id, &mut forwarder);
                match &cancel_result {
                    Ok(_) => {
                        if crate::diag::debug_enabled() {
                            crate::diag::log!("[dispatch] session_cancel emitted coord_id={id}");
                        }
                    }
                    Err(err) => {
                        if crate::diag::debug_enabled() {
                            crate::diag::log!(
                                "[dispatch] session_cancel refused coord_id={id} reason={err}"
                            );
                        }
                        let _ = tx.send(RuntimeEvent::Error(format!(
                            "[rust-session] cancel failed (coord id={id}): {err}"
                        )));
                    }
                }
            }
        }
    }
}

fn report_live_reload_failure(
    tx: &Sender<RuntimeEvent>,
    repaint_notifier: Option<&RepaintNotifier>,
    err: &str,
) {
    let message = format!("[runtime] {err}; retaining last-good session settings");
    crate::diag::log!("{message}");
    let _ = tx.send(RuntimeEvent::Stderr(message));
    if let Some(notifier) = repaint_notifier {
        notifier();
    }
}
/// Combined builder for the production wiring: returns the action sink
/// AND the [`OnceLock`] the supervisor populates from the live
/// [`crate::hotkey::HotkeyHandle::coordinator_handle`] after install.
///
/// The constructed session owns an explicit worker-event output mode, so its
/// lifecycle lines reach [`RuntimeEvent::Worker`] without setting
/// `VOICEPI_WORKER_EVENTS` in the process environment. Process-boundary CLI
/// paths retain the documented compatibility gate.
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

/// Convert a panic from model loading, transcription, injection, or another
/// coordinator-dispatched action into a terminal runtime event. Installation
/// has its own panic boundary, but local Whisper loads lazily on the first
/// utterance, after the UI is already Running.
pub(super) fn terminal_panic_boundary(
    mut inner: CoordinatorActionSink,
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) -> CoordinatorActionSink {
    let mut terminated = false;
    Box::new(move |action| {
        if terminated {
            if crate::diag::trace_enabled() {
                crate::diag::log!(
                    "[runtime/trace] coordinator action ignored after terminal panic action={action:?}"
                );
            }
            return;
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner(action)));
        if let Err(payload) = outcome {
            terminated = true;
            let detail = super::in_process::stringify_panic(payload);
            let message = format!("native runtime coordinator panicked: {detail}");
            crate::diag::log!("[runtime] {message}");
            if crate::diag::debug_enabled() {
                crate::diag::log!(
                    "[runtime/debug] coordinator panic marked terminal; scheduling full teardown"
                );
            }
            let _ = tx.send(RuntimeEvent::Error(message));
            let _ = tx.send(RuntimeEvent::Exited { code: Some(1) });
            if let Some(notifier) = repaint_notifier.as_ref() {
                notifier();
            }
        }
    })
}

pub(crate) fn build_production_sink(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) -> (CoordinatorActionSink, Arc<OnceLock<CoordinatorHandle>>) {
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
                let inner = build_session_action_sink_with_live_overrides(
                    Arc::clone(&deps.session),
                    tx,
                    move |id| {
                        if let Some(handle) = coord_slot_for_signal.get() {
                            handle.send(CoordinatorEvent::ProcessingFinished(id));
                        }
                    },
                    repaint_notifier,
                    super::live_settings::LiveEnvOverrides::default(),
                    true,
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
                     model via `wd models download large-v3-turbo` \
                     to enable real transcription."
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

/// Strict variant of [`build_production_sink`] for the in-process
/// Phase B path: returns Err when the real backends cannot be
/// constructed instead of silently falling back to the PR 4 stub sink.
///
/// Phase B (`VOICEPI_DICTATE_ENGINE=rust`) promises auto-fallback to
/// the Python worker when the in-process runtime cannot service PTT.
/// The silent-stub fallback in [`build_production_sink`] defeats that
/// contract: a build missing `whisper-rs-local` / `audio-capture`, or
/// one where model resolution fails, would install a stub sink that
/// returns empty transcriptions on every PTT press. The advertised
/// fallback never triggers because `try_install` returns Ok. Codex P1
/// PR #519 in_process.rs:373.
///
/// Two failure modes are surfaced:
///
/// 1. **Feature missing** — the build lacks `whisper-rs-local` +
///    `rust-injection`, so [`super::rust_session_real_backends`] is
///    not compiled. Returns Err with a rebuild message.
/// 2. **Real backend init failed** — features are present but
///    `make_real_session` returned Err (missing Whisper model, cpal
///    device open failure, etc.). Returns Err
///    with the underlying error string.
///
/// Only compiled when the in-process runtime's feature gate
/// (`rust-hotkeys` + `rust-injection`) is on — the only caller is
/// [`super::in_process::install_supported`], which itself is
/// feature-gated. On stock builds the module's `try_install` stub
/// returns [`super::in_process::InProcessInstallError::FeaturesMissing`]
/// before ever needing this helper.
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub(crate) fn try_build_production_sink(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
    live_env_overrides: super::live_settings::LiveEnvOverrides,
    runtime: super::settings_snapshot::RuntimeSettingsSnapshot,
) -> std::result::Result<
    (
        CoordinatorActionSink,
        Arc<OnceLock<CoordinatorHandle>>,
        Arc<std::sync::atomic::AtomicBool>,
        super::supervisor::CaptureStop,
    ),
    String,
> {
    #[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
    {
        let coord_slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());
        let runtime_active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let config_path = live_env_overrides.config_path.as_deref();
        let deps = super::rust_session_real_backends::make_real_session_with_activity_and_settings(
            tx.clone(),
            repaint_notifier.clone(),
            Arc::clone(&runtime_active),
            &runtime,
            config_path,
        )?;
        let capture_stop = Arc::clone(&deps.capture_stop);
        let coord_slot_for_signal = Arc::clone(&coord_slot);
        let inner = build_session_action_sink_with_live_overrides(
            Arc::clone(&deps.session),
            tx.clone(),
            move |id| {
                if let Some(handle) = coord_slot_for_signal.get() {
                    handle.send(CoordinatorEvent::ProcessingFinished(id));
                }
            },
            repaint_notifier.clone(),
            live_env_overrides,
            true,
        );
        let mut inner = inner;
        let _deps_keepalive = deps;
        let owning_sink = move |action: CoordinatorAction| {
            let _keepalive = &_deps_keepalive;
            inner(action);
        };
        let sink = terminal_panic_boundary(Box::new(owning_sink), tx.clone(), repaint_notifier);
        Ok((sink, coord_slot, runtime_active, capture_stop))
    }
    #[cfg(not(all(feature = "whisper-rs-local", feature = "rust-injection")))]
    {
        // Consume unused args so the signature stays constant across
        // feature configs.
        let _ = (tx, repaint_notifier, live_env_overrides);
        Err(
            "rust-session real backends require the `whisper-rs-local` + \
             `rust-injection` cargo features (rebuild with `cargo build \
             --no-default-features --features shipping`)"
                .to_owned(),
        )
    }
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
