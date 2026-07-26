//! Live partial-transcription preview for [`super::DictateSession`].
//!
//! Rust port of `src/python/whisper_dictate/vp_preview.py` -- closes parity
//! blocker #4 on the engine assessment. While the user is still holding PTT,
//! a background worker periodically re-transcribes the accumulated buffer and
//! emits a `state="preview"` worker event so the UI's live pipeline card can
//! show the sentence growing. Strictly DISPLAY-ONLY: the preview never feeds
//! back into the final transcription, never touches dictionary /
//! post-processing / injection / history, and swallows its own errors so a
//! preview failure can never take the session down.
//!
//! # Cadence + gates (parity with `vp_preview.py`)
//!
//! * **Interval** -- fires every `preview_seconds` seconds
//!   (`VOICEPI_PREVIEW_SECONDS`, default `3`). `0` disables.
//! * **Fresh-audio gate** -- a tick that has fewer than
//!   [`MIN_NEW_AUDIO_S`] seconds of NEW audio since the previous
//!   preview is skipped, so short pauses do not re-transcribe an
//!   essentially unchanged buffer.
//! * **Sliding window** -- each tick decodes only the most recent
//!   [`PREVIEW_MAX_AUDIO_S`] seconds so cost is bounded on long
//!   utterances (Python's `PREVIEW_MAX_AUDIO_S` comment: O(n) per
//!   tick, O(n^2) over utterance, unbounded on CPU without this cap).
//! * **Text cap** -- emitted `text_preview` is truncated to
//!   [`PREVIEW_TEXT_CHARS`] chars (generous, wraps in the UI).
//! * **Non-blocking** -- a tick that overruns the interval is skipped,
//!   not queued; the next tick recomputes from the fresh buffer.
//! * **Stop wins** -- once the session calls
//!   [`PreviewEngine::notify_stop`] the worker drops the buffer and
//!   emits NO further previews until the next [`PreviewEngine::notify_start`],
//!   so the final-pass path can never race a stale preview event onto
//!   the wire.
//!
//! # Eligibility
//!
//! Only the LOCAL Whisper backend is preview-eligible (`PREVIEW_BACKENDS = ("whisper",)`
//! in `vp_preview.py`). The cloud (`stt_backend=openai`) backend is excluded
//! -- previews there would spam a paid API. The gate is enforced by NOT
//! wiring a [`PreviewEngine`] into the session on the cloud path (see
//! [`crate::runtime::rust_session_real_backends::make_real_session`]).
//!
//! # Thread model
//!
//! One long-lived worker thread per session, driven by an [`mpsc::channel`].
//! The session sends [`PreviewMsg::Start`] on PTT press, [`PreviewMsg::Frame`]
//! per captured chunk, [`PreviewMsg::Stop`] on PTT release / cancel; the
//! [`PreviewEngine`]'s `Drop` impl sends [`PreviewMsg::Shutdown`] and joins.
//! The worker owns its own accumulator buffer -- the session's frame buffer
//! is untouched -- so the audio hot path pays only one channel send per
//! frame (bounded allocation) and never blocks on preview transcribe cost.

use std::io::Write;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::dictate::events::{emit_status, StatusEvent, WorkerStatus};

/// Fresh-audio gate: a preview needs at least this many seconds of NEW
/// audio since the previous preview to be worth transcribing again.
/// Mirrors `vp_preview.MIN_NEW_AUDIO_S`.
pub const MIN_NEW_AUDIO_S: f64 = 1.5;

/// Sliding-window cap: each tick decodes only the most recent
/// `PREVIEW_MAX_AUDIO_S` seconds of audio. Bounds per-tick cost on long
/// utterances. Mirrors `vp_preview.PREVIEW_MAX_AUDIO_S`.
pub const PREVIEW_MAX_AUDIO_S: f64 = 15.0;

/// Text cap for the emitted `text_preview` field. Mirrors
/// `vp_preview.PREVIEW_TEXT_CHARS`.
pub const PREVIEW_TEXT_CHARS: usize = 600;

/// Errors a [`PreviewBackend::transcribe_partial`] call can surface.
/// Non-fatal: the worker logs at most once per session and continues.
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    /// Underlying model invocation failed.
    #[error("preview backend error: {0}")]
    Backend(String),
}

/// The cheap partial-transcribe seam the preview worker calls once per tick.
///
/// A production impl (see `crate::dictate::backends::WhisperLocalTranscribeBackend`
/// behind the `whisper-rs-local` cargo feature) shares its model instance with
/// the session's final-pass [`crate::dictate::TranscribeBackend`] so the
/// preview does not double resident memory. `Send + Sync` because the trait
/// object lives inside the worker thread AND the session may hold its own
/// clone.
pub trait PreviewBackend: Send + Sync {
    /// Run a partial transcription on `pcm` at `sample_rate`. Returns the
    /// decoded text (may be empty; treated as "nothing to show"). Called at
    /// most once per interval; failures are swallowed by the worker.
    fn transcribe_partial(&self, pcm: &[f32], sample_rate: u32) -> Result<String, PreviewError>;
}

/// Payload the preview sink receives once per successful tick.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewEmission {
    /// Decoded text (already truncated to [`PreviewEngineConfig::text_chars`]).
    pub text: String,
    /// Total captured audio at the moment of the tick, rounded to 2 dp
    /// (mirrors Python's `round(samples / capture_rate, 2)`).
    pub recording_s: f64,
}

/// Sink the preview worker calls once per emitted preview. The production
/// wiring routes through [`stderr_preview_sink`], which writes a
/// `state="preview"` worker event via [`emit_status`]; tests capture the
/// emissions into a `Vec`.
pub type PreviewSink = Arc<dyn Fn(PreviewEmission) + Send + Sync>;

/// Build the production preview sink: emits each preview as a
/// `state="preview"` worker event on stderr using the same
/// [`crate::dictate::events`] emitter every other worker event goes
/// through. Respects the `VOICEPI_WORKER_EVENTS` env-gate exactly like
/// the session's own emitter, so a supervisor that opted out sees no
/// preview lines either.
pub fn stderr_preview_sink() -> PreviewSink {
    Arc::new(|emission: PreviewEmission| {
        let event = build_preview_status(&emission);
        let mut stderr = std::io::stderr().lock();
        let _ = emit_status(&mut stderr, &event);
        let _ = stderr.flush();
    })
}

/// Build the `StatusEvent` a preview emits. Exposed so the production sink
/// and the unit tests share the exact same field-shape assembly -- if this
/// drifts, the UI's live preview card breaks.
pub fn build_preview_status(emission: &PreviewEmission) -> StatusEvent {
    let mut event = StatusEvent::new(WorkerStatus::Preview);
    event.extras.insert(
        "text_preview".into(),
        serde_json::Value::from(emission.text.clone()),
    );
    event.extras.insert(
        "recording_s".into(),
        serde_json::json!(round2(emission.recording_s)),
    );
    event
}

/// Runtime configuration for a [`PreviewEngine`]. Built from
/// `VOICEPI_PREVIEW_SECONDS` via [`Self::from_seconds`]; `None` disables the
/// engine entirely (session builds no worker thread).
#[derive(Debug, Clone)]
pub struct PreviewEngineConfig {
    /// Wall-clock interval between ticks. Mirrors Python's `preview_seconds`.
    pub interval: Duration,
    /// PCM sample rate the frames the session pushes are at. Used to convert
    /// the [`MIN_NEW_AUDIO_S`] / [`PREVIEW_MAX_AUDIO_S`] second-based gates
    /// into sample counts.
    pub sample_rate: u32,
    /// Fresh-audio gate (seconds). Defaults to [`MIN_NEW_AUDIO_S`].
    pub min_new_audio_s: f64,
    /// Sliding-window cap (seconds). Defaults to [`PREVIEW_MAX_AUDIO_S`].
    pub max_audio_s: f64,
    /// Text cap for the emitted preview. Defaults to [`PREVIEW_TEXT_CHARS`].
    pub text_chars: usize,
}

impl PreviewEngineConfig {
    /// Resolve a config from the user's `preview_seconds` setting and a
    /// sample rate. Returns `None` when previews are disabled
    /// (`seconds <= 0`), matching Python's `preview_enabled` gate.
    pub fn from_seconds(seconds: f64, sample_rate: u32) -> Option<Self> {
        if seconds <= 0.0 {
            return None;
        }
        Some(Self {
            interval: Duration::from_secs_f64(seconds),
            sample_rate,
            min_new_audio_s: MIN_NEW_AUDIO_S,
            max_audio_s: PREVIEW_MAX_AUDIO_S,
            text_chars: PREVIEW_TEXT_CHARS,
        })
    }
}

/// Handle the session holds. Sending on `tx` is fire-and-forget; the worker
/// drops messages it cannot service (e.g. a `Frame` arriving before `Start`)
/// so the session's hot path stays non-blocking.
pub struct PreviewEngine {
    tx: Sender<PreviewMsg>,
    handle: Option<JoinHandle<()>>,
}

/// Messages the session sends to the worker thread.
enum PreviewMsg {
    /// PTT press: reset the accumulator + arm the fresh-audio timer.
    Start,
    /// One chunk of captured PCM. Appended to the worker's buffer while
    /// recording.
    Frame(Vec<f32>),
    /// PTT release / cancel: stop emitting previews until the next `Start`.
    Stop,
    /// `Drop` guard: exit the loop.
    Shutdown,
}

impl PreviewEngine {
    /// Spawn the worker thread. `backend` runs each tick's transcribe; `sink`
    /// receives the emissions.
    ///
    /// Production callers pass an `Arc<WhisperLocalTranscribeBackend>` cloned
    /// from the session's own transcribe backend so the model instance is
    /// shared -- no double RAM. See
    /// [`crate::runtime::rust_session_real_backends::make_real_session`].
    pub fn spawn(
        backend: Arc<dyn PreviewBackend>,
        config: PreviewEngineConfig,
        sink: PreviewSink,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("whisper-dictate-preview".to_owned())
            .spawn(move || preview_loop(rx, backend, config, sink))
            .expect("spawn whisper-dictate-preview thread");
        Self {
            tx,
            handle: Some(handle),
        }
    }

    /// Called by the session at the start of every recording (after the
    /// state flips to Recording). Resets the worker's accumulator and arms
    /// the interval timer.
    pub fn notify_start(&self) {
        let _ = self.tx.send(PreviewMsg::Start);
    }

    /// Called by the session per `push_frame`. Forwards a copy of the
    /// frame to the worker's own accumulator (no shared-buffer locking on
    /// the audio hot path).
    pub fn push_frame(&self, frame: &[f32]) {
        let _ = self.tx.send(PreviewMsg::Frame(frame.to_vec()));
    }

    /// Called by the session at the top of `stop_and_transcribe` / `cancel`
    /// -- BEFORE the final pass runs -- so no further previews land on the
    /// wire while the final transcription is happening.
    pub fn notify_stop(&self) {
        let _ = self.tx.send(PreviewMsg::Stop);
    }
}

impl Drop for PreviewEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(PreviewMsg::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Pure state the worker keeps between messages. Split out of the thread
/// loop so the fresh-audio + sliding-window logic is unit-testable without a
/// running thread / backend / sink.
pub(crate) struct PreviewState {
    config: PreviewEngineConfig,
    recording: bool,
    buf: Vec<f32>,
    last_preview_samples: usize,
}

impl PreviewState {
    pub(crate) fn new(config: PreviewEngineConfig) -> Self {
        Self {
            config,
            recording: false,
            buf: Vec::new(),
            last_preview_samples: 0,
        }
    }

    pub(crate) fn on_start(&mut self) {
        self.recording = true;
        self.buf.clear();
        self.last_preview_samples = 0;
    }

    pub(crate) fn on_stop(&mut self) {
        self.recording = false;
        self.buf.clear();
        self.last_preview_samples = 0;
    }

    pub(crate) fn on_frame(&mut self, frame: &[f32]) {
        if self.recording {
            self.buf.extend_from_slice(frame);
        }
    }

    pub(crate) fn is_recording(&self) -> bool {
        self.recording
    }

    /// If a tick should fire now (recording AND fresh-audio gate cleared),
    /// return `(windowed_pcm, total_captured_samples)` and stamp the
    /// last-preview watermark. `total_captured_samples` is the FULL captured
    /// length (before the sliding-window trim) so the caller can report
    /// real elapsed audio in `recording_s`.
    pub(crate) fn take_tick(&mut self) -> Option<(Vec<f32>, usize)> {
        if !self.recording {
            return None;
        }
        let total = self.buf.len();
        let min_new = seconds_to_samples(self.config.min_new_audio_s, self.config.sample_rate);
        if total.saturating_sub(self.last_preview_samples) < min_new {
            return None;
        }
        let max = seconds_to_samples(self.config.max_audio_s, self.config.sample_rate);
        let start = if max > 0 && total > max {
            total - max
        } else {
            0
        };
        let pcm = self.buf[start..].to_vec();
        self.last_preview_samples = total;
        Some((pcm, total))
    }
}

/// Convert `seconds` at `sample_rate` Hz into a sample count. Clamps
/// negatives to 0 so a mis-configured negative gate never underflows.
fn seconds_to_samples(seconds: f64, sample_rate: u32) -> usize {
    if seconds <= 0.0 {
        return 0;
    }
    (seconds * sample_rate as f64) as usize
}

/// Worker-thread body. Pulled out of `PreviewEngine::spawn` so the loop is a
/// free function (no captures beyond the args) and reads top-down.
fn preview_loop(
    rx: mpsc::Receiver<PreviewMsg>,
    backend: Arc<dyn PreviewBackend>,
    config: PreviewEngineConfig,
    sink: PreviewSink,
) {
    // Fallback sleep for the between-recordings idle window: the worker is
    // parked on rx.recv_timeout(...) with this duration when not recording,
    // so a Shutdown / Start message wakes it promptly regardless.
    const IDLE_POLL: Duration = Duration::from_secs(1);

    let mut state = PreviewState::new(config.clone());
    let mut error_logged = false;
    let mut next_tick = Instant::now() + config.interval;

    loop {
        // Fire a tick FIRST when we are past the deadline -- this way a
        // fast-arriving stream of Frame messages does not starve the tick
        // (we always drain the deadline check between messages).
        if state.is_recording() {
            let now = Instant::now();
            if now >= next_tick {
                run_tick(&mut state, &backend, &config, &sink, &mut error_logged);
                next_tick = Instant::now() + config.interval;
            }
        }

        let timeout = if state.is_recording() {
            next_tick.saturating_duration_since(Instant::now())
        } else {
            IDLE_POLL
        };

        match rx.recv_timeout(timeout) {
            Ok(PreviewMsg::Start) => {
                state.on_start();
                error_logged = false;
                next_tick = Instant::now() + config.interval;
            }
            Ok(PreviewMsg::Frame(frame)) => {
                state.on_frame(&frame);
            }
            Ok(PreviewMsg::Stop) => {
                state.on_stop();
            }
            Ok(PreviewMsg::Shutdown) => return,
            Err(RecvTimeoutError::Timeout) => {
                // Loop -- top of the loop fires the tick.
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// One tick attempt: pull the windowed buffer from the state, ask the
/// backend for a partial, emit through the sink. All failure paths are
/// swallowed (logged once per session at the outer layer). Kept `pub(crate)`
/// so the tests can drive it directly without spinning up the worker thread.
pub(crate) fn run_tick(
    state: &mut PreviewState,
    backend: &Arc<dyn PreviewBackend>,
    config: &PreviewEngineConfig,
    sink: &PreviewSink,
    error_logged: &mut bool,
) {
    let Some((pcm, total_samples)) = state.take_tick() else {
        return;
    };
    match backend.transcribe_partial(&pcm, config.sample_rate) {
        Ok(text) if !text.trim().is_empty() => {
            // Re-check recording: a Stop that arrived while the backend was
            // busy must suppress this emission (Python's `if not owner.recording:
            // return`). take_tick() only fires when recording, and the check
            // here catches a Stop that arrived mid-transcribe.
            if !state.is_recording() {
                return;
            }
            let recording_s = round2(total_samples as f64 / f64::from(config.sample_rate).max(1.0));
            let text = truncate_chars(&text, config.text_chars);
            sink(PreviewEmission { text, recording_s });
        }
        Ok(_) => {
            // Empty text -- backend produced nothing to show. Skip silently.
        }
        Err(err) => {
            if !*error_logged {
                eprintln!("[preview] failed: {err}");
                *error_logged = true;
            }
        }
    }
}

/// Truncate `text` to at most `chars` characters. Empty string when the
/// input has none. Kept local so the preview module does not pull in the
/// wider `text` helpers (which do more work -- Python's `_compact_text`
/// also normalises whitespace; the preview cap is a hard length limit).
fn truncate_chars(text: &str, chars: usize) -> String {
    if chars == 0 {
        return String::new();
    }
    let mut it = text.chars();
    let head: String = (&mut it).take(chars).collect();
    if it.next().is_some() {
        // There were more characters than the cap allowed; return the head.
        head
    } else {
        text.to_owned()
    }
}

/// Round to 2 decimal places, matching Python's `round(x, 2)`. Duplicated
/// from `wire.rs` so this module has no cross-module private dep.
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn config_ms(interval_ms: u64, sample_rate: u32) -> PreviewEngineConfig {
        PreviewEngineConfig {
            interval: Duration::from_millis(interval_ms),
            sample_rate,
            min_new_audio_s: MIN_NEW_AUDIO_S,
            max_audio_s: PREVIEW_MAX_AUDIO_S,
            text_chars: PREVIEW_TEXT_CHARS,
        }
    }

    /// One second of silent 16 kHz mono PCM (matches
    /// `MIN_NEW_AUDIO_S = 1.5` when pushed in two pieces).
    fn one_second_pcm() -> Vec<f32> {
        vec![0.0f32; 16_000]
    }

    /// Capturing sink used by the thread-driven tests.
    fn capturing_sink() -> (PreviewSink, Arc<Mutex<Vec<PreviewEmission>>>) {
        let captured: Arc<Mutex<Vec<PreviewEmission>>> = Arc::new(Mutex::new(Vec::new()));
        let handle = Arc::clone(&captured);
        let sink: PreviewSink = Arc::new(move |em| {
            handle.lock().unwrap().push(em);
        });
        (sink, captured)
    }

    /// Wait up to `total` for `pred` to hold, polling every 5 ms. Avoids
    /// arbitrary sleeps in tests that watch the worker thread emit.
    fn wait_until<F: FnMut() -> bool>(total: Duration, mut pred: F) -> bool {
        let start = Instant::now();
        while start.elapsed() < total {
            if pred() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        pred()
    }

    struct StubBackend {
        text: String,
        calls: AtomicUsize,
    }

    impl StubBackend {
        fn new(text: &str) -> Arc<Self> {
            Arc::new(Self {
                text: text.to_owned(),
                calls: AtomicUsize::new(0),
            })
        }
    }

    impl PreviewBackend for StubBackend {
        fn transcribe_partial(&self, _pcm: &[f32], _sr: u32) -> Result<String, PreviewError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.text.clone())
        }
    }

    struct FailingBackend {
        calls: AtomicUsize,
    }

    impl FailingBackend {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
            })
        }
    }

    impl PreviewBackend for FailingBackend {
        fn transcribe_partial(&self, _pcm: &[f32], _sr: u32) -> Result<String, PreviewError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(PreviewError::Backend("boom".to_owned()))
        }
    }

    // ── pure-state tests: fresh-audio gate + sliding window ─────────────────

    /// Ports `vp_preview.MIN_NEW_AUDIO_S` behaviour to Rust: N seconds of new
    /// audio (1.5) is the tick threshold, so 1 s of frames triggers 0
    /// previews, another 1 s (total 2 s) triggers 1, another 1 s (total 3 s)
    /// gate-skips (delta 1 s < 1.5 s), another 1 s (total 4 s) fires again.
    #[test]
    fn fresh_audio_gate_matches_python_min_new_audio_s() {
        let mut state = PreviewState::new(config_ms(1000, 16_000));
        state.on_start();

        // 1 s of frames: below the 1.5 s fresh-audio gate.
        state.on_frame(&one_second_pcm());
        assert!(
            state.take_tick().is_none(),
            "1 s < MIN_NEW_AUDIO_S (1.5 s) must skip"
        );

        // Total 2 s: delta = 2 s >= 1.5 s -> tick fires.
        state.on_frame(&one_second_pcm());
        let (pcm, total) = state.take_tick().expect("2 s clears the gate");
        assert_eq!(total, 2 * 16_000);
        assert_eq!(pcm.len(), 2 * 16_000, "below window cap -> full buffer");

        // Total 3 s: delta since last preview = 1 s -> gate skips again.
        state.on_frame(&one_second_pcm());
        assert!(state.take_tick().is_none(), "delta 1 s < 1.5 s must skip");

        // Total 4 s: delta = 2 s -> tick fires.
        state.on_frame(&one_second_pcm());
        let (_pcm, total) = state.take_tick().expect("delta 2 s clears the gate");
        assert_eq!(total, 4 * 16_000);
    }

    /// After stop the accumulator drops and any subsequent take_tick is
    /// suppressed -- final-pass path takes over. Mirrors Python's
    /// `if not self._owner.recording: break`.
    #[test]
    fn take_tick_after_stop_returns_none() {
        let mut state = PreviewState::new(config_ms(1000, 16_000));
        state.on_start();
        state.on_frame(&vec![0.0f32; 4 * 16_000]);
        assert!(state.take_tick().is_some());

        state.on_stop();
        state.on_frame(&vec![0.0f32; 4 * 16_000]); // dropped, not recording
        assert!(
            state.take_tick().is_none(),
            "no tick after stop until next start"
        );
    }

    /// Sliding-window cap: buffers longer than PREVIEW_MAX_AUDIO_S seconds
    /// are trimmed to the recent tail. `total_samples` stays uncapped so
    /// `recording_s` keeps tracking real elapsed audio.
    #[test]
    fn take_tick_windows_to_max_audio_seconds() {
        let sr = 16_000u32;
        let mut config = config_ms(500, sr);
        config.max_audio_s = 2.0; // trim to last 2 s for a short test
        let mut state = PreviewState::new(config);
        state.on_start();

        // 5 s of audio.
        state.on_frame(&vec![0.0f32; 5 * sr as usize]);
        let (pcm, total) = state.take_tick().expect("well above the gate");
        assert_eq!(total, 5 * sr as usize, "total is uncapped");
        assert_eq!(
            pcm.len(),
            2 * sr as usize,
            "windowed to the last max_audio_s seconds"
        );
    }

    // ── run_tick error/empty behaviour ──────────────────────────────────────

    #[test]
    fn run_tick_error_logs_once_and_does_not_emit() {
        let (sink, captured) = capturing_sink();
        let backend: Arc<dyn PreviewBackend> = FailingBackend::new() as Arc<dyn PreviewBackend>;
        let config = config_ms(500, 16_000);

        let mut state = PreviewState::new(config.clone());
        state.on_start();
        // Push enough audio to clear the fresh-audio gate.
        state.on_frame(&vec![0.0f32; 2 * 16_000]);
        let mut error_logged = false;
        run_tick(&mut state, &backend, &config, &sink, &mut error_logged);

        assert!(
            captured.lock().unwrap().is_empty(),
            "no emission on failure"
        );
        assert!(
            error_logged,
            "error flag set so subsequent failures stay quiet"
        );

        // Another tick with more audio -- gate cleared, backend still fails,
        // still no emission (and no panic).
        state.on_frame(&vec![0.0f32; 2 * 16_000]);
        run_tick(&mut state, &backend, &config, &sink, &mut error_logged);
        assert!(captured.lock().unwrap().is_empty());
    }

    #[test]
    fn run_tick_empty_backend_output_does_not_emit() {
        let (sink, captured) = capturing_sink();
        let backend: Arc<dyn PreviewBackend> = StubBackend::new("   ");
        let config = config_ms(500, 16_000);

        let mut state = PreviewState::new(config.clone());
        state.on_start();
        state.on_frame(&vec![0.0f32; 2 * 16_000]);
        let mut error_logged = false;
        run_tick(&mut state, &backend, &config, &sink, &mut error_logged);
        assert!(
            captured.lock().unwrap().is_empty(),
            "whitespace-only text is skipped"
        );
    }

    // ── payload wire shape ──────────────────────────────────────────────────

    /// Pins the emitted status-event shape byte-equivalent to Python's
    /// `_emit_worker_event("status", state="preview", text_preview=..., recording_s=...)`:
    /// `state="preview"`, extras carry `text_preview` (string) and
    /// `recording_s` (float, 2 dp). If ANY of those drift the UI's
    /// live preview card breaks.
    #[test]
    fn preview_event_payload_shape_matches_python() {
        let emission = PreviewEmission {
            text: "hej verden".to_owned(),
            recording_s: 1.2345,
        };
        let event = build_preview_status(&emission);
        assert_eq!(event.state, WorkerStatus::Preview);
        assert_eq!(event.state.as_wire_str(), "preview");
        assert_eq!(
            event.extras.get("text_preview").and_then(|v| v.as_str()),
            Some("hej verden")
        );
        // Rounded to 2 dp, matching Python's round(samples/rate, 2).
        assert_eq!(
            event.extras.get("recording_s").and_then(|v| v.as_f64()),
            Some(1.23)
        );
    }

    /// Text longer than `text_chars` is truncated char-safely (not byte-
    /// unsafely, so multi-byte UTF-8 does not panic).
    #[test]
    fn preview_text_is_truncated_char_safely() {
        assert_eq!(truncate_chars("hello", 3), "hel");
        assert_eq!(truncate_chars("æøå", 2), "æø");
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("anything", 0), "");
    }

    // ── engine lifecycle (thread-driven) ────────────────────────────────────

    #[test]
    fn engine_emits_after_interval_when_gate_clears() {
        let (sink, captured) = capturing_sink();
        let backend: Arc<dyn PreviewBackend> = StubBackend::new("hej");
        let mut config = config_ms(30, 16_000);
        config.min_new_audio_s = 0.0; // let the first tick fire
        let engine = PreviewEngine::spawn(Arc::clone(&backend), config, sink);
        engine.notify_start();
        engine.push_frame(&one_second_pcm());

        let got = wait_until(Duration::from_secs(2), || {
            !captured.lock().unwrap().is_empty()
        });
        assert!(got, "engine must emit at least one preview");
        let first = captured.lock().unwrap()[0].clone();
        assert_eq!(first.text, "hej");
        assert!(first.recording_s > 0.0);

        drop(engine); // joins the worker
    }

    #[test]
    fn engine_stops_emitting_after_notify_stop() {
        let (sink, captured) = capturing_sink();
        let backend = StubBackend::new("word");
        let backend_dyn: Arc<dyn PreviewBackend> = Arc::clone(&backend) as Arc<dyn PreviewBackend>;
        let mut config = config_ms(20, 16_000);
        config.min_new_audio_s = 0.0;
        let engine = PreviewEngine::spawn(backend_dyn, config, sink);

        engine.notify_start();
        engine.push_frame(&one_second_pcm());
        wait_until(Duration::from_millis(500), || {
            !captured.lock().unwrap().is_empty()
        });
        assert!(!captured.lock().unwrap().is_empty(), "warm-up emission");

        engine.notify_stop();
        let baseline = captured.lock().unwrap().len();
        // Push more audio AFTER stop: worker must drop it, no further
        // emissions -- final-pass path is the only source of truth once
        // stop was signalled.
        engine.push_frame(&one_second_pcm());
        engine.push_frame(&one_second_pcm());
        thread::sleep(Duration::from_millis(200));
        assert_eq!(
            captured.lock().unwrap().len(),
            baseline,
            "no previews after notify_stop"
        );

        drop(engine);
    }

    #[test]
    fn engine_swallows_backend_errors() {
        let (sink, captured) = capturing_sink();
        let backend = FailingBackend::new();
        let backend_dyn: Arc<dyn PreviewBackend> = Arc::clone(&backend) as Arc<dyn PreviewBackend>;
        let mut config = config_ms(20, 16_000);
        config.min_new_audio_s = 0.0;
        let engine = PreviewEngine::spawn(backend_dyn, config, sink);

        engine.notify_start();
        engine.push_frame(&one_second_pcm());
        // Wait long enough that at least one tick was attempted.
        thread::sleep(Duration::from_millis(200));
        engine.notify_stop();

        assert!(
            captured.lock().unwrap().is_empty(),
            "failing backend never emits"
        );
        assert!(
            backend.calls.load(Ordering::SeqCst) >= 1,
            "backend was invoked at least once (worker did not crash on the first failure)"
        );

        drop(engine);
    }

    #[test]
    fn from_seconds_disabled_when_zero_or_negative() {
        assert!(PreviewEngineConfig::from_seconds(0.0, 16_000).is_none());
        assert!(PreviewEngineConfig::from_seconds(-1.0, 16_000).is_none());
        assert!(PreviewEngineConfig::from_seconds(3.0, 16_000).is_some());
    }
}
