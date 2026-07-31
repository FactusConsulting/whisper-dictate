//! Preview engine: config, worker thread, per-tick state machine.
//!
//! Split out of the pre-modularity-fix single-file `preview.rs` (Codex P1
//! #608 preview.rs:457) alongside sibling modules for the backend
//! ([`super::backend`]) and the emission surface ([`super::emission`]).
//!
//! The engine owns the worker thread. `PreviewEngine::spawn` boots it;
//! `notify_start` / `push_frame` / `notify_stop` are the session-facing
//! handles; `Drop` shuts the thread down cleanly. Everything else in
//! this module is worker-side (the message enum, the loop, the pure
//! tick state, the tick attempt helper) and is `pub(crate)` at most --
//! the public API surface stays the [`PreviewEngine`] +
//! [`PreviewEngineConfig`] pair.
//!
//! # Stop-race fix (Codex P1 #608 preview.rs:245)
//!
//! `notify_stop` used to send only a channel message. `transcribe_partial`
//! can block for hundreds of ms, so a stop that arrived mid-tick was
//! invisible to the worker until the backend returned -- by which time
//! the current tick had already fired an emission, racing the session's
//! final `utterance` event onto the wire.
//!
//! The fix is an [`std::sync::atomic::AtomicBool`] shared between the
//! session-facing handle and the worker's [`run_tick`]:
//!
//! - `notify_stop` sets the flag SYNCHRONOUSLY (before the channel send
//!   returns) with `Ordering::Release`.
//! - `run_tick` re-reads the flag AFTER `transcribe_partial` returns
//!   (before touching the sink) with `Ordering::Acquire`; if set, it
//!   drops the tick's payload on the floor.
//! - `PreviewMsg::Start` clears the flag (`Ordering::Release`) so the
//!   next recording is unaffected.
//!
//! The pure `PreviewState::is_recording()` guard is retained as a
//! belt-and-braces post-check for the case where the worker has
//! already consumed the `PreviewMsg::Stop` message from its channel
//! -- both signals now suppress a stale emission.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::backend::PreviewBackend;
use super::emission::{round2, truncate_chars, PreviewEmission, PreviewSink};

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
    /// Stop-race guard (Codex P1 #608 preview.rs:245). Set synchronously
    /// by [`Self::notify_stop`] so a tick's post-`transcribe_partial`
    /// re-check ([`run_tick`]) can suppress the emission BEFORE the
    /// worker has drained the pending `Stop` message from its channel.
    /// Cloned into the worker on [`Self::spawn`]; `Arc<AtomicBool>` so
    /// the session's handle and the worker share a single storage cell.
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// Messages the session sends to the worker thread.
pub(crate) enum PreviewMsg {
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
        let stop_flag = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop_flag);
        let handle = thread::Builder::new()
            .name("whisper-dictate-preview".to_owned())
            .spawn(move || preview_loop(rx, backend, config, sink, worker_stop))
            .expect("spawn whisper-dictate-preview thread");
        Self {
            tx,
            stop_flag,
            handle: Some(handle),
        }
    }

    /// Called by the session at the start of every recording (after the
    /// state flips to Recording). Resets the worker's accumulator and arms
    /// the interval timer.
    pub fn notify_start(&self) {
        // Clear the stop-race flag BEFORE the worker sees `Start` so a
        // tick that fires against the new recording sees a clean flag.
        // Release so the worker's Acquire load in `run_tick` observes a
        // false value even if the previous stop's Release is still
        // in-flight in the memory subsystem.
        self.stop_flag.store(false, Ordering::Release);
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
    ///
    /// Sets the shared stop flag SYNCHRONOUSLY (Codex P1 #608
    /// preview.rs:245): even if the worker is mid-`transcribe_partial`
    /// and has not yet consumed the [`PreviewMsg::Stop`] message from
    /// its channel, [`run_tick`]'s post-transcribe Acquire load will
    /// see the flag and drop the pending emission on the floor.
    pub fn notify_stop(&self) {
        self.stop_flag.store(true, Ordering::Release);
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
pub(crate) fn seconds_to_samples(seconds: f64, sample_rate: u32) -> usize {
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
    stop_flag: Arc<AtomicBool>,
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
                run_tick(
                    &mut state,
                    &backend,
                    &config,
                    &sink,
                    &mut error_logged,
                    &stop_flag,
                );
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
///
/// `stop_flag` is the [`PreviewEngine`]'s shared stop-race guard: after
/// `transcribe_partial` returns we re-check it with `Ordering::Acquire`
/// (paired with the [`PreviewEngine::notify_stop`] `Release` store) and
/// suppress the emission when set. This closes the Codex P1 #608
/// preview.rs:245 race where a stop signalled mid-transcribe would
/// otherwise race a stale preview event past the final `utterance`
/// event.
pub(crate) fn run_tick(
    state: &mut PreviewState,
    backend: &Arc<dyn PreviewBackend>,
    config: &PreviewEngineConfig,
    sink: &PreviewSink,
    error_logged: &mut bool,
    stop_flag: &AtomicBool,
) {
    let Some((pcm, total_samples)) = state.take_tick() else {
        return;
    };
    match backend.transcribe_partial(&pcm, config.sample_rate) {
        Ok(text) if !text.trim().is_empty() => {
            // Codex P1 #608 preview.rs:245 fix: re-check the SHARED stop
            // flag first (Acquire pairs with notify_stop's Release). The
            // worker may not yet have consumed the pending Stop message
            // from its channel -- the atomic bridges that gap so a stop
            // signalled while the backend was busy still suppresses the
            // emission before it can race the final utterance event.
            if stop_flag.load(Ordering::Acquire) {
                return;
            }
            // Belt + braces: the pure state may have already flipped if
            // the worker DID consume Stop between messages. Keeps the
            // pre-fix behaviour that Python's `if not owner.recording:
            // return` also covered.
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
                crate::diag::log!("[preview] failed: {err}");
                *error_logged = true;
            }
        }
    }
}
