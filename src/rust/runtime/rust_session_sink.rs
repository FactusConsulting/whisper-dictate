//! Wire the hotkey coordinator's
//! [`crate::hotkey::coordinator::CoordinatorAction`] sink into a
//! [`crate::dictate::DictateSession`] so PTT press/release drives
//! `session.start()` / `stop_and_transcribe()` / `cancel(epoch)`.
//!
//! The supervisor and `wd dictate-run` always install this sink through
//! [`try_build_production_sink`], which drives the real backends built by
//! [`super::rust_session_real_backends`]. There is no stub fallback: when
//! the backends cannot be constructed (feature missing, no resolvable
//! model, device failure) the builder returns `Err` and the runtime
//! refuses to start, so a PTT press never silently produces an empty
//! transcription.
//!
//! # Feature gating
//!
//! [`try_build_production_sink`] is compiled with `rust-hotkeys` +
//! `rust-injection` (the in-process runtime's gate); the real-backend
//! branch inside it needs `whisper-rs-local` + `rust-injection` as well.
//! Without that pair the action-sink chain below is only reachable from
//! tests, so the module tolerates dead code on such builds -- the same
//! shape the `capture_*` modules use.
//!
//! Unit tests live in the sibling `rust_session_sink_tests.rs`,
//! `rust_session_sink_coverage_tests.rs` and
//! `rust_session_sink_e2e_tests.rs` modules; the stub backends they drive
//! live in [`super::test_support`].
#![cfg_attr(
    not(all(feature = "whisper-rs-local", feature = "rust-injection")),
    allow(dead_code)
)]

use std::io::Write;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use crate::dictate::{DictateSession, InjectBackend, TranscribeBackend};
use crate::hotkey::coordinator::{CoordinatorAction, CoordinatorEvent, CoordinatorHandle};
use crate::runtime::{RepaintNotifier, RuntimeEvent, WorkerEvent};

/// Prefix every `[worker-event] {…}` line carries. Mirrors the
/// `WORKER_EVENT_PREFIX` const in `runtime.rs`; kept local so this
/// module compiles standalone. `pub(super)` so the sibling
/// [`super::rust_session_sink_tests`] module can spell the
/// prefix literally in its assertions.
pub(super) const WORKER_EVENT_PREFIX: &str = "[worker-event] ";

// ── sink factory ─────────────────────────────────────────────────────────────

/// Test-only shorthand for [`build_session_action_sink_with_live_overrides`]
/// without live overrides, runtime boundaries or native capture: drives
/// `session` from [`CoordinatorAction`]s and reports each completed stop
/// to `on_processing_finished` with the recording id.
///
/// Each worker-event line the session writes is forwarded onto `tx`:
/// `[worker-event] {…}` lines are parsed into [`RuntimeEvent::Worker`]
/// (so consumers like the egui log card key off the same variant they
/// see for session-sink events); anything else lands as
/// [`RuntimeEvent::Stderr`].
#[cfg(test)]
pub(super) fn build_session_action_sink<T, I, F>(
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
        CoordinatorSignals {
            processing_finished: on_processing_finished,
            recording_abandoned: |_| {},
        },
        repaint_notifier,
        super::live_settings::LiveEnvOverrides::default(),
        false,
        None,
    )
}

/// `recording_capture` opens the microphone after a recording starts and
/// closes it after the release tail, before transcription, or on cancel
/// (#323). `None` for sessions without native capture.
///
/// `signals` reports back to the coordinator (see [`CoordinatorSignals`]).
pub(super) fn build_session_action_sink_with_live_overrides<T, I, F, A>(
    session: Arc<Mutex<DictateSession<T, I>>>,
    tx: Sender<RuntimeEvent>,
    signals: CoordinatorSignals<F, A>,
    repaint_notifier: Option<RepaintNotifier>,
    live_env_overrides: super::live_settings::LiveEnvOverrides,
    runtime_boundaries: bool,
    recording_capture: Option<super::recording_capture::RecordingCaptureHandle>,
) -> impl FnMut(CoordinatorAction) + Send + 'static
where
    T: TranscribeBackend + Send + 'static,
    I: InjectBackend + Send + 'static,
    F: Fn(u64) + Send + Sync + 'static,
    A: Fn(u64) + Send + Sync + 'static,
{
    // Start/stop/cancel (including opening and closing the microphone around
    // each recording, #323) live in `session_recording_actions`.
    let mut actions = super::session_recording_actions::RecordingActions::new(
        session,
        tx,
        repaint_notifier,
        live_env_overrides,
        runtime_boundaries,
        recording_capture,
    );
    move |action: CoordinatorAction| {
        if crate::diag::debug_enabled() {
            crate::diag::log!("[dispatch] coordinator_action={action:?}");
        }
        match action {
            CoordinatorAction::StartRecording(id) => {
                if actions.start(id) {
                    // No microphone: end the coordinator's recording stage so
                    // the next press opens again instead of being consumed as
                    // a stop (toggle mode).
                    (signals.recording_abandoned)(id);
                }
            }
            CoordinatorAction::StopAndTranscribe(id) => {
                actions.stop(id);
                (signals.processing_finished)(id);
                if crate::diag::debug_enabled() {
                    crate::diag::log!("[dispatch] processing_finished_signalled coord_id={id}");
                }
            }
            CoordinatorAction::CancelRecording(id) => actions.cancel(id),
        }
    }
}
/// Coordinator feedback for the action sink raised by the production
/// builder: `ProcessingFinished(id)` after a stop, and
/// [`CoordinatorHandle::abort_recording`] when a recording was abandoned
/// because no microphone could be opened. The abort is a flag the
/// coordinator consumes as soon as the sink returns -- before it reads the
/// next queued event -- so a retry press the user made while the device was
/// opening starts a new recording instead of being consumed as a stop in
/// toggle mode (#323).
pub(super) struct CoordinatorSignals<F, A> {
    /// Runs after a stop completed, with the recording id.
    pub(super) processing_finished: F,
    /// Runs when a recording was begun but no microphone could be opened.
    pub(super) recording_abandoned: A,
}

/// Shared rendezvous between the action sink (built first) and the live
/// [`CoordinatorHandle`] (which only exists once the hotkey listener is
/// installed).
///
/// The listener can install — and a first press can fail to open a
/// microphone — before the supervisor publishes the handle. An abort raised
/// in that window is retained here and flushed by [`Self::publish`], so the
/// coordinator never stays parked in `Stage::Recording` waiting for a signal
/// that a toggle release would never produce (#323).
pub(crate) struct CoordinatorLink {
    slot: OnceLock<CoordinatorHandle>,
    /// Signals raised before publication, guarded by the same lock as
    /// publication: without it a signal could observe an empty slot,
    /// publication could flush nothing, and the retention could then land
    /// with nobody left to consume it.
    pending: Mutex<PendingSignals>,
}

/// Signals the sink raised while the coordinator handle was unpublished.
#[derive(Default)]
struct PendingSignals {
    /// Recording id of a failed-open abort.
    abort: Option<u64>,
    /// Recording id of a completed stop. Dropping this would wedge the
    /// coordinator in `Stage::Processing` for the rest of the runtime.
    finished: Option<u64>,
}

impl CoordinatorLink {
    pub(crate) fn new() -> Self {
        Self {
            slot: OnceLock::new(),
            pending: Mutex::new(PendingSignals::default()),
        }
    }

    /// Publish the live handle and immediately raise whatever was retained
    /// from before publication. Returns false when a handle was already
    /// published.
    pub(crate) fn publish(&self, handle: CoordinatorHandle) -> bool {
        let mut pending = lock_pending(&self.pending);
        if self.slot.set(handle).is_err() {
            return false;
        }
        let published = self.slot.get().expect("just published");
        if let Some(id) = pending.abort.take() {
            published.abort_recording(id);
        }
        if let Some(id) = pending.finished.take() {
            published.send(CoordinatorEvent::ProcessingFinished(id));
        }
        true
    }

    /// Record that recording `id` never began. Applied to the coordinator at
    /// once when it is already wired, otherwise retained for publication.
    /// The lock makes the two paths mutually exclusive; the id lets the
    /// coordinator ignore an abort whose recording is long gone.
    fn abort_recording(&self, id: u64) {
        let mut pending = lock_pending(&self.pending);
        match self.slot.get() {
            Some(handle) => {
                pending.abort = None;
                handle.abort_recording(id);
            }
            None => pending.abort = Some(id),
        }
    }

    /// Report that the stop for recording `id` finished. Retained the same
    /// way as an abort: a full press/release cycle can complete before the
    /// supervisor publishes the handle (empty recording, zero release tail),
    /// and dropping it would leave the coordinator in `Stage::Processing`
    /// with every later press deferred.
    fn processing_finished(&self, id: u64) {
        let mut pending = lock_pending(&self.pending);
        match self.slot.get() {
            Some(handle) => {
                pending.finished = None;
                handle.send(CoordinatorEvent::ProcessingFinished(id));
            }
            None => pending.finished = Some(id),
        }
    }
}

fn lock_pending(pending: &Mutex<PendingSignals>) -> std::sync::MutexGuard<'_, PendingSignals> {
    pending.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub(super) fn coordinator_signals(
    link: &Arc<CoordinatorLink>,
) -> CoordinatorSignals<impl Fn(u64) + Send + Sync + 'static, impl Fn(u64) + Send + Sync + 'static>
{
    let finished_link = Arc::clone(link);
    let abandoned_link = Arc::clone(link);
    CoordinatorSignals {
        processing_finished: move |id| finished_link.processing_finished(id),
        recording_abandoned: move |id| abandoned_link.abort_recording(id),
    }
}

/// Boxed action-sink closure handed back from [`try_build_production_sink`].
/// Aliased so clippy's `type_complexity` lint stays quiet (the tuple
/// return type otherwise breaches the threshold). The `Box<dyn …>`
/// indirection lets [`terminal_panic_boundary`] wrap the inner closure
/// without naming its capture type.
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

/// Build the production action sink: returns the sink, the
/// [`CoordinatorLink`] the supervisor publishes the live
/// [`crate::hotkey::HotkeyHandle::coordinator_handle`] into after install,
/// the runtime-active flag, and the capture-stop handle.
///
/// The session is built by
/// [`super::rust_session_real_backends::make_real_session_with_activity_and_settings`]
/// and owns an explicit worker-event output mode, so its lifecycle lines
/// reach [`RuntimeEvent::Worker`] without setting `VOICEPI_WORKER_EVENTS`
/// in the process environment. Process-boundary CLI paths retain the
/// documented compatibility gate.
///
/// `repaint_notifier` is the UI's wake-up callback (the same one
/// `RuntimeSupervisor::stream_lines` runs after every event it enqueues).
/// Threaded here so the in-process session's events do not sit in the
/// channel until some unrelated repaint -- on Windows with the window
/// minimised, the egui tick does not fire without an explicit nudge.
///
/// Returns `Err` when the real backends cannot be constructed; there is no
/// stub fallback, because an in-process runtime that reports ready and then
/// returns empty transcriptions on every PTT press would hide a broken
/// install. Two failure modes are surfaced:
///
/// 1. **Feature missing** -- the build lacks `whisper-rs-local` +
///    `rust-injection`, so [`super::rust_session_real_backends`] is
///    not compiled. Returns Err with a rebuild message.
/// 2. **Real backend init failed** -- features are present but the
///    constructor returned Err (missing Whisper model, cpal device open
///    failure, etc.). Returns Err with the underlying error string.
///
/// Only compiled when the in-process runtime's feature gate
/// (`rust-hotkeys` + `rust-injection`) is on -- the callers are
/// [`super::in_process::install_supported`] and `wd dictate-run`, which
/// are gated the same way. On stock builds the module's `try_install` stub
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
        Arc<CoordinatorLink>,
        Arc<std::sync::atomic::AtomicBool>,
        super::supervisor::CaptureStop,
    ),
    String,
> {
    #[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
    {
        let coord_slot: Arc<CoordinatorLink> = Arc::new(CoordinatorLink::new());
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
        let inner = build_session_action_sink_with_live_overrides(
            Arc::clone(&deps.session),
            tx.clone(),
            coordinator_signals(&coord_slot),
            repaint_notifier.clone(),
            live_env_overrides,
            true,
            deps.recording_capture.clone(),
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
        let _ = (tx, repaint_notifier, live_env_overrides, runtime);
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
/// `RuntimeSupervisor::repaint_notifier`).
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
