//! Session-owned per-utterance state and backend boundaries.
//!
//! Recording, cancellation, live/profile settings, and the ordered transcript
//! pipeline live in focused children. This module owns the state and keeps
//! the public session API stable; tests exercise traits without real devices.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use serde_json::{json, Value};

pub(crate) mod audio_loss;

#[cfg(test)]
mod history_retention_tests;
pub mod history_sink;
pub mod metrics_sink;
pub(crate) mod path_util;
pub mod preview;
pub mod types;
mod wire;

#[cfg(test)]
#[path = "path_util_tests.rs"]
mod path_util_tests;
#[cfg(test)]
mod tests_history_sink;
#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests_live_settings;
#[cfg(test)]
mod tests_metrics_sink;
#[cfg(test)]
mod tests_ported;
// Wave 5 follow-up (rust-target-profile-matching branch): tests for the
// per-utterance target-profile matcher wire-up (Python parity for
// `_profiled_config` in `vp_dictate._start`).
#[cfg(test)]
mod tests_profile;
#[cfg(test)]
mod tests_support;
#[cfg(test)]
mod tests_transitions;
#[cfg(test)]
#[path = "mod_tests.rs"]
mod transcription_boundary_tests;
#[cfg(test)]
mod wire_tests;

#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) use history_sink::history_sink_from_app_settings;
pub use history_sink::{
    effective_history_settings, history_sink_from_settings, EffectiveHistorySettings, HistorySink,
    JsonlHistorySink, NoopHistorySink, ReloadingHistorySink,
};
#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) use metrics_sink::metrics_sink_from_app_settings;
pub use metrics_sink::{
    effective_metrics_settings, metrics_sink_from_settings, EffectiveMetricsSettings,
    JsonlMetricsSink, MetricsSink, NoopMetricsSink, ReloadingMetricsSink,
};
pub use preview::{
    build_preview_status, stderr_preview_sink, PreviewBackend, PreviewEmission, PreviewEngine,
    PreviewEngineConfig, PreviewError, PreviewSink, MIN_NEW_AUDIO_S, PREVIEW_MAX_AUDIO_S,
    PREVIEW_TEXT_CHARS,
};
pub use types::{
    InjectBackend, InjectError, PostProcessBackend, PostProcessOutcome, PostRedaction,
    SessionConfig, SessionError, SessionState, TranscribeBackend, TranscribeError,
    TranscribeResult, UtteranceOutcome, SR,
};

use crate::dictate::profile::{AppliedProfile, ProfileMatcher};
use crate::platform::foreground_window::{ForegroundWindowProbe, WindowInfo};

mod builders;
#[cfg(test)]
mod builders_tests;
mod cancellation;
#[cfg(test)]
mod cancellation_tests;
mod dictionary_policy;
#[cfg(test)]
mod dictionary_policy_tests;
mod limits;
#[cfg(test)]
mod limits_tests;
mod profile_policy;
#[cfg(test)]
mod profile_policy_tests;
mod recording;
#[cfg(test)]
mod recording_tests;
mod settings;
mod transcription;
#[cfg(test)]
mod transcription_tests;

#[cfg(test)]
use limits::max_record_samples_from_env;
use limits::{max_record_samples, parse_max_record_seconds};
use profile_policy::emit_profile_status;

pub(crate) fn normalize_gate_reason(gate: &str) -> &'static str {
    let lowered = gate.to_ascii_lowercase();
    if lowered.contains("too quiet") {
        return "too_quiet";
    }
    if lowered.contains("no speech") {
        return "no_speech";
    }
    "empty"
}
/// Per-utterance state machine. Owns the capture buffer and the
/// transcribe / inject backends; emits status events through a
/// caller-supplied writer.
///
/// See the module docs for the design rationale. See `tests_ported.rs`
/// for the six characterisation tests ported from
/// `src/python/tests/test_dictate_loop.py` and `tests_transitions.rs`
/// for the supplementary state-transition invariants.
pub struct DictateSession<T: TranscribeBackend, I: InjectBackend> {
    state: SessionState,
    /// Session-owned worker-event policy. Standalone/process-boundary sessions
    /// preserve the compatibility env gate; the native in-process runtime opts
    /// in explicitly without mutating process-global state.
    worker_event_output: crate::dictate::events::WorkerEventOutput,
    /// Captured PCM at the model's sample rate (16 kHz mono). In this
    /// PR `push_frame` already-resampled samples; PR 3 owns the
    /// channel-select + resample at consumption.
    frame_buf: Vec<f32>,
    /// Effective per-utterance recording ceiling in 16 kHz samples.
    /// Reloaded from `VOICEPI_MAX_RECORD_S` at every PTT start so the
    /// native direct-to-session audio pump cannot bypass the safety cap.
    max_record_samples: Option<usize>,
    /// One-shot diagnostic gate for the current utterance.
    max_record_cap_logged: bool,
    /// Final capture diagnostics transferred only after its forwarder joins.
    recording_audio_loss: Option<audio_loss::RecordingAudioLoss>,
    /// Monotonic recording generation. Bumped on every `start()` so
    /// the chord-race guard in `cancel()` can detect a stale request.
    /// See `vp_dictate.py:140-147 + 665-684` for the exact race.
    epoch: u64,
    config: SessionConfig,
    /// Device currently feeding this session. Kept separate from `config` so
    /// an in-memory fallback never mutates the saved selector and profile/live
    /// setting resets cannot overwrite the effective runtime device.
    effective_audio_device: Arc<RwLock<String>>,
    transcribe: T,
    inject: I,
    /// Optional LLM post-processing pass applied to the final transcript
    /// BEFORE the format-command layer and injection (Python's
    /// `postprocess -> format -> inject` order). `None` -- the default --
    /// skips the pass entirely and suppresses the `post-processing`
    /// status, so a session built with [`Self::new`] behaves exactly as
    /// before this seam existed. Set via [`Self::with_post_process`].
    post_process: Option<Box<dyn PostProcessBackend + Send>>,
    /// Optional provider of the replacement table that rewrites the transcript
    /// FIRST -- before post-processing, formatting and injection -- mirroring
    /// Python's `_dictionary_runtime(raw_text)` step in
    /// `vp_transcribe._transcribe_detail` (replacements are applied to the
    /// decoded text before it leaves the transcribe path). Resolved once per
    /// utterance via [`crate::dictionary::DictionaryProvider::current`], so a
    /// [`crate::dictionary::ReloadingDictionary`] (set via
    /// [`Self::with_reloading_dictionary`]) live-reloads file/settings edits
    /// between utterances while a [`crate::dictionary::StaticDictionary`] (set
    /// via [`Self::with_dictionary`]) keeps a fixed table. `None` -- the
    /// default -- applies no replacements, so a session built with
    /// [`Self::new`] behaves exactly as before this seam existed.
    dictionary: Option<Box<dyn crate::dictionary::DictionaryProvider + Send>>,
    /// Sink that plays the audible press/release cues (Rust port of
    /// `vp_feedback.play_cue`). Always present so the state machine
    /// never has to `if let Some(...)` on the hot path; the default in
    /// [`Self::new`] is [`crate::dictate::feedback::NoOpCueSink`] so
    /// existing tests neither depend on the audio subsystem nor emit
    /// sounds. Production wires [`crate::dictate::feedback::SystemCueSink`]
    /// via [`Self::with_cue_sink`], which reads
    /// `VOICEPI_FEEDBACK_SOUNDS` on every call for parity with the
    /// Python engine's live env-driven gate.
    cue_sink: Box<dyn crate::dictate::feedback::CueSink + Send>,
    /// Optional history-JSONL sink that receives the completed utterance
    /// event alongside the worker-event emitter, mirroring Python's
    /// `_record_utterance_event` (which calls `_emit_worker_event` AND
    /// `append_record_sinks` on the same event dict). `None` -- the default
    /// -- writes no history, so a session built with [`Self::new`] behaves
    /// exactly as before this seam existed and the pre-existing tests
    /// stay byte-identical. Sink errors are non-fatal: the implementation
    /// logs a warning to stderr and the session continues, matching the
    /// `try / except OSError` around `append_record_sinks` in
    /// `vp_dictate.py::_record_utterance_event`.
    history_sink: Option<Box<dyn HistorySink + Send>>,
    /// Optional per-utterance target-profile matcher.
    profile_matcher: Option<Box<dyn ProfileMatcher>>,
    /// Foreground-window probe consulted at [`Self::start`]. Always present.
    foreground_probe: Box<dyn ForegroundWindowProbe>,
    /// Immutable base copy of [`Self::config`] for per-utterance profile overlay wipe.
    base_config: SessionConfig,
    /// Live schema settings reloaded by the runtime before each utterance. Profile
    /// values overlay this map for one utterance; an empty map preserves the
    /// construction-time behavior used by standalone/unit-test sessions.
    live_settings: std::collections::BTreeMap<String, String>,
    /// The profile the matcher resolved for the current / most-recent utterance.
    active_profile: Option<AppliedProfile>,
    /// Foreground-window snapshot captured at [`Self::start`] alongside
    /// [`Self::active_profile`]. Held so the completed utterance's
    /// `target_title` / `target_process` fields (Python parity:
    /// `_inject_target_title` / `_inject_target_process`) reflect the
    /// window the user was focused on when they pressed PTT -- not the
    /// window they happened to be on when injection ran. `None` when no
    /// profile matcher is attached (unit tests, `simulate-session`).
    /// Codex P1 #606 metrics-schema follow-up.
    active_window: Option<WindowInfo>,
    /// Optional live-preview engine that emits `state="preview"` worker events
    /// during recording (see PR #608 / `preview` module).
    preview: Option<PreviewEngine>,
    /// Optional metrics-JSONL sink (parity blocker #6). See #606.
    metrics_sink: Option<Box<dyn MetricsSink + Send>>,
    /// Audio ducker driven at PTT press (start) / PTT release (stop / cancel).
    /// Rust port of Python's `vp_audio_ducking.AudioDucker` (parity blocker #2).
    audio_ducker: Box<dyn crate::dictate::audio_ducking::AudioDucker + Send>,
    /// Run the configured command-hook on completed utterance payloads. Kept
    /// opt-in so pure/simulated sessions never launch external processes.
    command_hook: bool,
    /// Use [`SessionConfig`] values instead of the compatibility env/default
    /// config resolver for command hooks.
    command_hook_uses_owned_settings: bool,
    /// Optional supervisor lifecycle gate for the command hook. Injection and
    /// hooks share this gate so Stop closes every outward side effect even
    /// when transcription was already in flight.
    command_hook_activity: Option<Arc<AtomicBool>>,
}
impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
    /// Build a fresh session. The session starts in
    /// [`SessionState::Idle`] with an empty buffer and `epoch == 0`, and
    /// with no post-processor (use [`Self::with_post_process`] to attach
    /// one).
    pub fn new(transcribe: T, inject: I, config: SessionConfig) -> Self {
        let effective_audio_device = Arc::new(RwLock::new(config.audio_device.clone()));
        Self {
            state: SessionState::Idle,
            worker_event_output: crate::dictate::events::WorkerEventOutput::Environment,
            frame_buf: Vec::new(),
            max_record_samples: None,
            max_record_cap_logged: false,
            recording_audio_loss: None,
            epoch: 0,
            base_config: config.clone(),
            live_settings: std::collections::BTreeMap::new(),
            config,
            effective_audio_device,
            transcribe,
            inject,
            post_process: None,
            dictionary: None,
            cue_sink: Box::new(crate::dictate::feedback::NoOpCueSink),
            history_sink: None,
            profile_matcher: None,
            foreground_probe: Box::new(
                crate::platform::foreground_window::FixedForegroundWindow::default(),
            ),
            active_profile: None,
            active_window: None,
            preview: None,
            metrics_sink: None,
            audio_ducker: Box::new(crate::dictate::audio_ducking::NoOpAudioDucker),
            command_hook: false,
            command_hook_uses_owned_settings: false,
            command_hook_activity: None,
        }
    }

    /// Emit worker events unconditionally for this owned in-process session.
    /// Compatibility CLI/process-boundary callers keep the default env gate.
    pub(crate) fn with_worker_events_enabled(mut self) -> Self {
        self.worker_event_output = crate::dictate::events::WorkerEventOutput::Enabled;
        self
    }

    /// The capture-backend / audio-device / capture-channels extras
    /// every status event carries. Empty strings and zero values are
    /// dropped by [`wire::emit_status`], so an unconfigured session
    /// emits a clean minimal event.
    fn capture_extras(&self) -> [(&'static str, Value); 3] {
        let effective_audio_device = self
            .effective_audio_device
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        [
            (
                "capture_backend",
                Value::from(self.config.capture_backend.clone()),
            ),
            ("audio_device", Value::from(effective_audio_device)),
            (
                "capture_channels",
                Value::from(self.config.capture_channels),
            ),
        ]
    }
}
