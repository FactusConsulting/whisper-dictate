//! Pure-logic per-utterance state machine for the live PTT dictation
//! loop. Mirrors `src/python/whisper_dictate/vp_dictate.py::Dictate`'s
//! per-utterance lifecycle (start → push frames → stop/transcribe →
//! inject, with cancel) but with NO audio capture, NO model loading and
//! NO real injection: every side-effecting boundary goes through a
//! trait-bound mock so unit tests run without cpal / whisper / enigo.
//!
//! Wave 5 PR 2 of issue #348. The audio-route (PR 3), hotkey wiring
//! (PR 4) and full Rust supervisor (PR 5+) are what consume this — there
//! is no production caller in this PR yet. Adding the state machine in
//! isolation lets the per-utterance transition logic be unit-tested
//! end-to-end (the six characterisation tests ported from
//! `src/python/tests/test_dictate_loop.py`) before the I/O layer lands.
//!
//! # Why a trait-bound design
//!
//! `vp_dictate.py` weaves capture / transcribe / inject side-effects
//! into the same per-utterance method that owns the state machine, which
//! is why `test_dictate_loop.py` has to build a `Dictate` via
//! `object.__new__` and monkey-patch six boundary functions to test the
//! orchestration. Splitting capture / transcribe / inject out as traits
//! up-front gives us the same testability without the monkey-patching
//! gymnastics, and is the shape PR 3/4 already need anyway because cpal
//! lives in a `cfg(feature = "audio-in-rust")` module.
//!
//! # Module layout
//!
//! - [`types`] — public trait boundaries + result / state / error / config
//!   types. Re-exported through this module.
//! - [`wire`] — the narrow `[worker-event] {…}\n` line emitter the
//!   session uses for status / utterance events. Will be swapped for
//!   the richer `crate::dictate::events` emitter from PR 1 (#412) by
//!   PR 3, once both PRs are in `main`.
//! - [`history_sink`] — the optional local-JSONL history writer that
//!   ports `vp_history.append_record_sinks`'s write side to Rust so the
//!   in-process engine records every completed utterance to the same
//!   file the Python engine writes to today.
//! - [`metrics_sink`] — sibling to `history_sink`: the metrics-JSONL
//!   writer that fans the utterance event out to the machine-readable
//!   metrics file (`metrics_jsonl`, gated on `inject_json`), matching
//!   Python's `vp_history.append_record_sinks` metrics branch.
//! - [`tests_support`] — `cfg(test)` test backends + helpers shared
//!   across the test files.
//! - [`tests_ported`] — the six characterisation tests ported from
//!   `src/python/tests/test_dictate_loop.py`.
//! - [`tests_transitions`] — supplementary state-transition invariants.
//! - [`tests_history_sink`] — `HistorySink` wiring integration tests.
//! - [`tests_metrics_sink`] — `MetricsSink` wiring integration tests.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

pub mod history_sink;
pub mod metrics_sink;
mod path_util;
pub mod preview;
pub mod types;
mod wire;

#[cfg(test)]
#[path = "path_util_tests.rs"]
mod path_util_tests;
#[cfg(test)]
mod tests_history_sink;
#[cfg(test)]
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
mod wire_tests;

pub use history_sink::{
    effective_history_settings, history_sink_from_settings, EffectiveHistorySettings, HistorySink,
    JsonlHistorySink, NoopHistorySink, ReloadingHistorySink,
};
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

/// Translate the backend's free-form gate text (as `result.gate` carries
/// it -- e.g. `"input too quiet: -42 dBFS"`, `"no speech contrast: 0.02"`)
/// into one of the three reason tokens the worker-event consumers / UI
/// cards switch on: `"too_quiet"`, `"no_speech"`, `"empty"`. Mirrors the
/// Python mapper in `vp_transcribe.py` (substring-based, ASCII-cased).
/// Codex P2 #413 mod.rs:284 (round 2 follow-up to the `gate` field
/// landed in round 1).
/// Emit one `[worker-event] event=status state=profile` line describing
/// the profile match resolved at the top of [`DictateSession::start`].
/// Mirrors the Python worker's `[profile] active: NAME` print in
/// `vp_dictate._profiled_config` -- the JSON form so consumers (egui log
/// card, telemetry, tests) key off the same wire shape they already read
/// for `recording` / `transcribing`.
///
/// Emitted for every utterance where a profile matcher is attached
/// (whether or not a profile actually fired) so the observer sees the
/// negative case too (`active_profile=""`), matching Python's
/// `f"[profile] active: {profile_name or 'default'}"`. The line is
/// suppressed entirely when no matcher is attached so tests that
/// pre-date this seam keep their exact event traces.
fn emit_profile_status<W: Write>(
    writer: &mut W,
    window: &WindowInfo,
    applied: Option<&AppliedProfile>,
) -> Result<(), SessionError> {
    let profile_name = applied
        .and_then(|p| p.name.as_deref())
        .unwrap_or_default()
        .to_owned();
    // Only emit when there is something worth reporting -- either a
    // resolved profile OR at least one probe field. This suppresses noise
    // in test sessions that don't opt into the matcher (there the probe
    // stays a FixedForegroundWindow::default() and the applied slot is
    // None).
    if profile_name.is_empty() && window.is_empty() {
        return Ok(());
    }
    let extras: [(&'static str, Value); 4] = [
        ("active_profile", Value::from(profile_name)),
        (
            "target_title",
            Value::from(window.title.clone().unwrap_or_default()),
        ),
        (
            "target_process",
            Value::from(window.process.clone().unwrap_or_default()),
        ),
        (
            "target_id",
            Value::from(window.target_id.clone().unwrap_or_default()),
        ),
    ];
    wire::emit_status(writer, "profile", &extras)
}

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
    /// Monotonic recording generation. Bumped on every `start()` so
    /// the chord-race guard in `cancel()` can detect a stale request.
    /// See `vp_dictate.py:140-147 + 665-684` for the exact race.
    epoch: u64,
    config: SessionConfig,
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
        Self {
            state: SessionState::Idle,
            frame_buf: Vec::new(),
            max_record_samples: None,
            max_record_cap_logged: false,
            epoch: 0,
            base_config: config.clone(),
            live_settings: std::collections::BTreeMap::new(),
            config,
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
            command_hook_activity: None,
        }
    }

    /// Enable the configured command hook for production utterances. The hook
    /// resolver reads env/config on every call, so enabling/disabling it in
    /// Settings applies at the next utterance boundary.
    pub fn with_command_hook(mut self) -> Self {
        self.command_hook = true;
        self
    }

    /// Enable command hooks while the shared runtime lifecycle remains active.
    pub fn with_command_hook_activity(mut self, runtime_active: Arc<AtomicBool>) -> Self {
        self.command_hook = true;
        self.command_hook_activity = Some(runtime_active);
        self
    }

    fn command_hook_enabled(&self) -> bool {
        let active = self
            .command_hook_activity
            .as_ref()
            .is_none_or(|gate| gate.load(Ordering::Acquire));
        if self.command_hook && !active && crate::diag::debug_enabled() {
            crate::diag::log!(
                "[runtime/debug] command hook suppressed because lifecycle gate is closed"
            );
        }
        self.command_hook && active
    }

    /// Attach a live-preview engine that will emit `state="preview"` worker
    /// events during recording (see [`preview`] for the cadence + suppression
    /// contract, and [`PreviewEngineConfig::from_seconds`] for the disabled
    /// gate). Passing this is opt-in: the production wiring only attaches an
    /// engine on the LOCAL Whisper backend, matching Python's
    /// `PREVIEW_BACKENDS = ("whisper",)` cloud-cost guard.
    pub fn with_preview_engine(mut self, engine: PreviewEngine) -> Self {
        self.preview = Some(engine);
        self
    }

    /// Attach a preview engine only when one was actually constructed
    /// (i.e. `preview_seconds > 0` AND the backend is preview-eligible).
    /// Convenience wrapper so the runtime factory can call
    /// `.with_optional_preview_engine(...)` without pre-checking the Option.
    pub fn with_optional_preview_engine(mut self, engine: Option<PreviewEngine>) -> Self {
        if let Some(engine) = engine {
            self.preview = Some(engine);
        }
        self
    }

    /// Attach an audio ducker driven at PTT press (start) / PTT release
    /// (stop / cancel). Default is a silent no-op; production wiring
    /// (`make_real_session`) attaches
    /// [`crate::dictate::audio_ducking::SystemAudioDucker`], whose
    /// `from_env` constructor reads `VOICEPI_AUDIO_DUCKING` +
    /// `VOICEPI_AUDIO_DUCKING_LEVEL` (parity with Python's
    /// `vp_audio_ducking.AudioDucker.from_config()`). Closes parity blocker #2.
    pub fn with_ducker(
        mut self,
        ducker: Box<dyn crate::dictate::audio_ducking::AudioDucker + Send>,
    ) -> Self {
        self.audio_ducker = ducker;
        self
    }

    /// Attach an audible-cue sink played at PTT press (start) and PTT
    /// release (stop). The default sink is a silent no-op, so a
    /// caller who does not opt in is byte-for-byte identical to a
    /// session built before this seam existed. Production wiring
    /// (`make_real_session`) attaches
    /// [`crate::dictate::feedback::SystemCueSink`], which itself
    /// respects the `VOICEPI_FEEDBACK_SOUNDS` env-var gate on every
    /// call -- matching Python's `vp_feedback.play_cue`.
    pub fn with_cue_sink(
        mut self,
        sink: Box<dyn crate::dictate::feedback::CueSink + Send>,
    ) -> Self {
        self.cue_sink = sink;
        self
    }

    /// Attach a [`HistorySink`] so every completed utterance also lands in
    /// the local JSONL history file (parity with Python's
    /// `_record_utterance_event -> append_record_sinks`). `None` -- the
    /// default -- keeps the pre-existing no-write behaviour. Sink errors
    /// are non-fatal: the implementation logs a warning and the session
    /// continues, so a broken history file can never drop a dictation.
    ///
    /// Passing this is opt-in: production wiring only attaches a sink when
    /// the user has `history_enabled=true` (default) AND a resolvable
    /// history path -- see [`history_sink_from_settings`].
    pub fn with_history_sink(mut self, sink: Box<dyn HistorySink + Send>) -> Self {
        self.history_sink = Some(sink);
        self
    }

    /// Attach a history sink only when [`history_sink_from_settings`]
    /// resolves one (i.e. the user has not disabled history). Convenience
    /// wrapper so the real-backends factory can call `.with_optional_history_sink()`
    /// without pre-checking the Option itself.
    pub fn with_optional_history_sink(mut self, sink: Option<Box<dyn HistorySink + Send>>) -> Self {
        if let Some(sink) = sink {
            self.history_sink = Some(sink);
        }
        self
    }

    /// Attach a [`MetricsSink`] so every completed utterance also lands
    /// in the machine-readable metrics JSONL file (parity with Python's
    /// `_record_utterance_event -> append_record_sinks` metrics branch).
    /// `None` -- the default -- keeps the pre-existing no-write behaviour.
    /// Sink errors are non-fatal: the implementation logs a warning and the
    /// session continues, so a broken metrics file can never drop a
    /// dictation.
    ///
    /// Passing this is opt-in: production wiring only attaches a sink when
    /// the user has `inject_json=true` (Python `json_output`) AND a
    /// non-empty `metrics_jsonl` path -- see [`metrics_sink_from_settings`].
    pub fn with_metrics_sink(mut self, sink: Box<dyn MetricsSink + Send>) -> Self {
        self.metrics_sink = Some(sink);
        self
    }

    /// Attach a metrics sink only when [`metrics_sink_from_settings`]
    /// resolves one (i.e. the user has `inject_json=true` AND a non-empty
    /// `metrics_jsonl` path). Convenience wrapper so the real-backends
    /// factory can call `.with_optional_metrics_sink()` without pre-checking
    /// the Option itself.
    pub fn with_optional_metrics_sink(mut self, sink: Option<Box<dyn MetricsSink + Send>>) -> Self {
        if let Some(sink) = sink {
            self.metrics_sink = Some(sink);
        }
        self
    }

    /// Attach an LLM post-processing backend, returning the session so
    /// callers can chain it after [`Self::new`]. When set, a successful
    /// utterance runs `backend.post_process(text)` (emitting a
    /// `post-processing` status) before the format-command layer and
    /// injection. Passing this is opt-in: the production wiring only
    /// attaches a backend when the operator configured a post-processor
    /// (`VOICEPI_POST_PROCESSOR` != `none`).
    pub fn with_post_process(mut self, backend: Box<dyn PostProcessBackend + Send>) -> Self {
        self.post_process = Some(backend);
        self
    }

    /// Attach a dictionary whose replacement table rewrites the transcript
    /// BEFORE post-processing, formatting and injection -- mirroring Python's
    /// `_dictionary_runtime(raw_text)` step in `vp_transcribe._transcribe_detail`
    /// (replacements are applied to the decoded text before it leaves the
    /// transcribe path). Passing this is opt-in: the production wiring only
    /// attaches a dictionary when the configured one actually has replacements,
    /// so a session without one is byte-identical to before this seam existed.
    /// (Term-based prompt biasing -- the other half of dictionary support -- is
    /// applied at backend-config construction, not here.)
    pub fn with_dictionary(mut self, dictionary: crate::dictionary::Dictionary) -> Self {
        self.dictionary = Some(Box::new(crate::dictionary::StaticDictionary(dictionary)));
        self
    }

    /// Attach a live-reloading dictionary: the replacement table is re-read
    /// from `config.json` + the process env + the dictionary file(s) at each
    /// utterance boundary (cheap when unchanged, via an mtime+settings cache
    /// key), so a user editing their dictionary or changing the `dictionary*`
    /// live settings sees the change on the next utterance without restarting
    /// the app -- matching Python's per-utterance `_dictionary_runtime` and the
    /// config layer's `live` flag for those keys. `precedence` selects which
    /// source wins: [`crate::dictionary::ReloadPrecedence::ConfigFirst`] for the
    /// live worker session (config.json is the source of truth) or
    /// [`crate::dictionary::ReloadPrecedence::EnvFirst`] for the env-driven
    /// `simulate-session` CLI. The initial table is loaded now, so the first
    /// utterance already reflects the on-disk state. (Term-based prompt biasing
    /// -- the other half of dictionary support -- is still folded into the
    /// backend config at construction; its live-reload is a separate follow-up.)
    pub fn with_reloading_dictionary(
        mut self,
        precedence: crate::dictionary::ReloadPrecedence,
    ) -> Self {
        self.dictionary = Some(Box::new(crate::dictionary::ReloadingDictionary::new(
            precedence,
        )));
        self
    }

    /// Attach a session dictionary when it supplies transcript replacements.
    pub fn with_optional_dictionary(
        self,
        dictionary: crate::dictionary::SessionDictionary,
    ) -> Self {
        if dictionary.has_replacements() {
            let mut session = self;
            session.dictionary = Some(Box::new(crate::dictionary::StaticDictionary(
                dictionary.dictionary,
            )));
            session
        } else {
            self
        }
    }

    /// Attach a per-utterance target-profile matcher + foreground-window
    /// probe. Parity with the Python worker's
    /// `_capture_target_window` + `_profiled_config` pair (called from
    /// `_start` on every PTT press): each utterance the session probes
    /// the focused window, hands the resulting title / process pair to
    /// the matcher, and applies the returned setting overrides to the
    /// effective [`SessionConfig`] for THIS utterance only.
    ///
    /// Passing this is opt-in: production wiring pairs
    /// [`crate::dictate::profile::ReloadingProfileMatcher`] (re-reads
    /// `config.json` each utterance -- matches Python's
    /// `_reload_live_config_if_changed`) with
    /// [`crate::platform::foreground_window::SystemForegroundWindow`] (the
    /// per-OS probe). Tests plug the
    /// [`crate::dictate::profile::StaticProfileMatcher`] and
    /// [`crate::platform::foreground_window::FixedForegroundWindow`]
    /// deterministic implementations. A session without a matcher stays
    /// byte-identical to one built before this seam existed.
    ///
    /// The session applies `format_commands`, `min_record_seconds`, and
    /// `inject_mode` to the effective configuration. Backend hooks receive
    /// the complete settings map for their own per-utterance overrides.
    pub fn with_profile_matcher(
        mut self,
        matcher: Box<dyn ProfileMatcher>,
        probe: Box<dyn ForegroundWindowProbe>,
    ) -> Self {
        self.profile_matcher = Some(matcher);
        self.foreground_probe = probe;
        self
    }

    /// The profile the matcher resolved for the current / most-recent
    /// utterance. `None` when no matcher is attached, or when the matcher
    /// resolved to no match on the last [`Self::start`] call. Exposed for
    /// telemetry + follow-up backend wiring; the session itself only
    /// consumes the session-config subset.
    pub fn active_profile(&self) -> Option<&AppliedProfile> {
        self.active_profile.as_ref()
    }

    /// Whether a profile matcher has been attached via
    /// [`Self::with_profile_matcher`]. Exposed so the production factory's
    /// unit tests can assert the wire-up (Codex P1 #607) without the
    /// session having to leak its private matcher field.
    pub fn has_profile_matcher(&self) -> bool {
        self.profile_matcher.is_some()
    }

    /// Apply the resolved [`AppliedProfile`] to the effective
    /// [`SessionConfig`] for this utterance. Called from [`Self::start`]
    /// after probing + matching. Split out so the mapping between
    /// profile-setting keys and `SessionConfig` fields is easy to unit
    /// test and to extend.
    ///
    /// Reset semantics: `self.config` is refreshed from `self.base_config`
    /// first so a profile that fired for a PREVIOUS utterance cannot leak
    /// its overrides into the current one. Then each supported key from
    /// the profile settings map overwrites the matching field.
    ///
    /// Unsupported / unparseable values (e.g. `min_record_seconds="foo"`)
    /// are silently ignored -- matching Python's `_apply_effective_config`
    /// path where the caller trusts the settings validator upstream to
    /// have rejected bad data. Log noise on the PTT hot path is worse
    /// than a silent fall-through to the default.
    fn apply_active_profile(&mut self) {
        self.config = self.base_config.clone();
        // Empty map when no profile matched -- the backends need this so
        // they can RESET any overrides they applied for a PREVIOUS
        // utterance's profile (else a profile that fired for utterance N
        // would silently persist into N+1 when N+1 hits the wildcard /
        // default branch). Mirrors the config-reset done above for
        // `self.config`.
        let mut effective_settings = self.live_settings.clone();
        if let Some(profile) = self.active_profile.as_ref() {
            effective_settings.extend(profile.settings.clone());
        }
        let settings = &effective_settings;
        if let Some(value) = settings.get("format_commands") {
            let trimmed = value.trim();
            self.config.format_command_set = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            };
        }
        if let Some(value) = settings.get("min_record_seconds") {
            if let Ok(parsed) = value.trim().parse::<f64>() {
                self.config.min_record_seconds = parsed;
            }
        }
        if let Some(value) = settings.get("inject_mode") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                self.config.inject_mode = trimmed.to_owned();
            }
        }
        // Each backend picks the keys it understands and stores them for its
        // next call without rebuilding the session.
        self.transcribe.apply_profile_overrides(settings);
        self.inject.apply_profile_overrides(settings);
        if let Some(backend) = self.post_process.as_ref() {
            backend.apply_profile_overrides(settings);
        }
    }

    /// Probe the foreground window + resolve the matched profile. Split
    /// out so tests can drive the resolve path without going through
    /// `start()` (which owns the event-emission side effects).
    fn resolve_profile(&self) -> (WindowInfo, Option<AppliedProfile>) {
        let Some(matcher) = self.profile_matcher.as_ref() else {
            return (WindowInfo::default(), None);
        };
        let window = self.foreground_probe.probe();
        let applied = matcher.resolve(&window);
        if applied.is_none() {
            (window, None)
        } else {
            (window, Some(applied))
        }
    }

    /// Apply the attached dictionary's replacement table to `text`, returning
    /// the rewritten string and the per-replacement change records (for the
    /// utterance event's `dictionary_replacements` field). The table is
    /// resolved through the provider ([`crate::dictionary::DictionaryProvider::current`]),
    /// so a reloading provider re-reads it here at the utterance boundary. A
    /// `None` provider, an empty replacement table, or empty text is a
    /// passthrough (no changes); a replacement regex error keeps the original
    /// text (a replacement failure must never drop a dictation). Takes `&mut
    /// self` because the provider may mutate its reload cache.
    fn apply_dictionary(
        &mut self,
        text: &str,
    ) -> (
        String,
        Vec<crate::dictionary::ReplacementChange>,
        Option<String>,
    ) {
        match &mut self.dictionary {
            Some(provider) => {
                let dictionary = provider.current();
                let replacements = if text.is_empty() {
                    (text.to_owned(), Vec::new())
                } else {
                    dictionary
                        .apply_replacements(text)
                        .unwrap_or_else(|_| (text.to_owned(), Vec::new()))
                };
                let load_error = provider.take_load_error();
                (replacements.0, replacements.1, load_error)
            }
            None => (text.to_owned(), Vec::new(), None),
        }
    }

    /// Current state-machine phase. Exposed for tests and the
    /// supervisor's UI; the session itself is the source of truth.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Current recording epoch. Returned by [`Self::start`] and read by
    /// [`Self::cancel`] for the chord-race guard.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Replace the ambient live-setting overlay for the next utterance. The
    /// profile matcher still wins per key because [`Self::apply_active_profile`]
    /// overlays a matched profile after cloning this map.
    pub fn update_live_settings(&mut self, settings: std::collections::BTreeMap<String, String>) {
        self.live_settings = settings;
    }

    /// Re-set the per-session min-record floor in seconds. The
    /// `min_record_seconds` setting is `live: true` in
    /// `shared/config/settings_schema.json`; the audio
    /// route calls this on every successful
    /// [`crate::dictate::audio_route::AudioRoute::start_recording`]
    /// (after re-reading [`crate::dictate::audio_route::MIN_RECORD_ENV`])
    /// so a Settings save between PTT presses takes effect on the next
    /// recording without rebuilding the session. The skip helper still
    /// clamps the effective floor up to
    /// [`crate::dictate::skip::MIN_RECORD_FLOOR_S`] (0.3 s) regardless,
    /// so a misconfigured 0 still surfaces the misfire protection.
    /// Codex P2 #415 audio_route.rs:250 (round 7-D).
    pub fn update_min_record_seconds(&mut self, seconds: f64) {
        self.config.min_record_seconds = seconds;
        // Also update the base so a subsequent utterance whose profile
        // does NOT override `min_record_seconds` still sees the live-
        // reloaded value (rather than snapping back to the stale
        // construction-time floor when `apply_active_profile` resets
        // `self.config = self.base_config.clone()`). Codex-anticipated
        // guard on the profile seam introduced by
        // `rust-target-profile-matching`.
        self.base_config.min_record_seconds = seconds;
    }

    /// Read-only access to the transcribe backend. Tests use this to
    /// inspect what the session passed to the mock; production callers
    /// will rarely need it.
    pub fn transcribe_backend(&self) -> &T {
        &self.transcribe
    }

    /// Read-only access to the inject backend. Tests use this to assert
    /// what the session injected.
    pub fn inject_backend(&self) -> &I {
        &self.inject
    }

    /// Open a fresh utterance.
    ///
    /// Mirrors `vp_dictate.py::_start`:
    /// 1. early-return if a recording is already in flight (no events,
    ///    no state change — same guard as Python's `if self.recording`);
    /// 2. clear the frame buffer;
    /// 3. bump the recording epoch (the chord-race generation counter
    ///    — see `vp_dictate.py:140-147`);
    /// 4. emit `status=opening`;
    /// 5. transition to [`SessionState::Recording`] and emit
    ///    `status=recording` with capture backend / device / channels.
    ///
    /// Returns the new epoch so the caller (e.g. a chord-cancel
    /// dispatcher) can stamp the value and pass it back to
    /// [`Self::cancel`].
    pub fn start<W: Write>(&mut self, writer: &mut W) -> Result<u64, SessionError> {
        if !matches!(self.state, SessionState::Idle) {
            return Err(SessionError::AlreadyActive { state: self.state });
        }
        self.frame_buf.clear();
        self.max_record_samples = max_record_samples_from_env();
        self.max_record_cap_logged = false;
        if crate::diag::debug_enabled() {
            crate::diag::log!(
                "[runtime/debug] recording buffer reset max_record_samples={:?}",
                self.max_record_samples
            );
        }
        self.epoch = self.epoch.wrapping_add(1);
        let id = self.epoch;
        // Per-utterance target-profile resolution -- Python parity:
        // `_capture_target_window` + `_profiled_config(effective_config())`
        // called from `_start` BEFORE the "opening" event fires. Kept
        // ahead of the state flip so the emitted `[worker-event]
        // event=profile` line lands adjacent to the utterance it
        // applies to, and so the `apply_active_profile` reset happens
        // before `stop_and_transcribe` reads `self.config`. A None
        // matcher is a zero-cost no-op (see `resolve_profile`).
        let (window, applied) = self.resolve_profile();
        self.active_profile = applied;
        // Stash the window snapshot so the utterance event carries the
        // target the user was focused on at PTT-press (not at
        // inject-time). Codex P1 #606 metrics-schema follow-up.
        self.active_window = if window.is_empty() {
            None
        } else {
            Some(window.clone())
        };
        self.apply_active_profile();
        emit_profile_status(writer, &window, self.active_profile.as_ref())?;
        self.state = SessionState::Opening { id };
        // Restore Idle if status output fails; callers otherwise cannot start
        // a new recording. The epoch remains monotonic, so gaps are harmless.
        if let Err(e) = wire::emit_status(writer, "opening", &[]) {
            self.state = SessionState::Idle;
            return Err(e);
        }
        self.state = SessionState::Recording { id };
        if let Err(e) = wire::emit_status(writer, "recording", &self.capture_extras()) {
            self.state = SessionState::Idle;
            return Err(e);
        }
        // Audible press cue -- matches `vp_dictate.py::_start` (line
        // 589), which calls `play_cue("start")` AFTER the "listening..."
        // print / status flip. `NoOpCueSink` (the default) makes this
        // a no-op; production wires `SystemCueSink`, which itself
        // gates on `VOICEPI_FEEDBACK_SOUNDS`. Non-blocking + never
        // fails, so no error path is threaded here.
        self.cue_sink.play(crate::dictate::feedback::CueKind::Start);
        // Arm the live-preview worker for this recording (parity: `_start_preview`).
        if let Some(engine) = self.preview.as_ref() {
            engine.notify_start();
        }
        // Audio ducking -- matches vp_dictate.py::_start's
        // `self.audio_ducker.enter()` right before the capture handshake.
        // Infallible by trait contract; failures swallowed into a one-shot warning.
        self.audio_ducker.enter();
        Ok(id)
    }

    /// Append a chunk of post-resample, post-channel-select PCM to the
    /// capture buffer.
    ///
    /// Frames pushed while the session is not in [`SessionState::Recording`]
    /// are silently dropped — matching the Python capture mixin, which
    /// gates frame ingestion on `self.recording == True`. This makes the
    /// session safe to drive from a long-lived audio reader thread that
    /// outlives any single utterance.
    pub fn push_frame(&mut self, frame: &[f32]) {
        if matches!(self.state, SessionState::Recording { .. }) {
            let accepted_len = self
                .max_record_samples
                .map(|cap| cap.saturating_sub(self.frame_buf.len()).min(frame.len()))
                .unwrap_or(frame.len());
            let accepted = &frame[..accepted_len];
            self.frame_buf.extend_from_slice(accepted);
            // Forward a copy to the preview worker so it can accumulate its
            // own sliding-window buffer without locking on the session's
            // hot-path Vec. `push_frame` on the engine is a channel send;
            // if the receiver is missing (shouldn't happen while the engine
            // is alive) the message is silently dropped.
            if let Some(engine) = self.preview.as_ref() {
                engine.push_frame(accepted);
            }
            if accepted_len < frame.len() && !self.max_record_cap_logged {
                self.max_record_cap_logged = true;
                crate::diag::log!(
                    "[runtime] recording reached max_record_s; discarding additional audio cap_samples={} dropped_samples={}",
                    self.max_record_samples.unwrap_or(0),
                    frame.len() - accepted_len
                );
                if crate::diag::trace_enabled() {
                    crate::diag::log!(
                        "[runtime/trace] recording cap state buffered_samples={} incoming_samples={} accepted_samples={accepted_len}",
                        self.frame_buf.len(),
                        frame.len()
                    );
                }
            }
        }
    }

    /// Close the recording, decide skip / hallucination / inject, and
    /// emit the matching status + utterance events.
    ///
    /// Mirrors `vp_dictate.py::_stop_and_transcribe`:
    /// * empty buffer → `status=no_text reason=no_audio`,
    ///   returns [`UtteranceOutcome::NoAudio`].
    /// * buffer below the min-duration floor →
    ///   `status=no_text reason=too_short`, returns
    ///   [`UtteranceOutcome::Skipped`].
    /// * backend error or empty / hallucinated text →
    ///   `status=no_text reason=…`, returns [`UtteranceOutcome::NoText`].
    /// * success → inject, emit `event=utterance`, return
    ///   [`UtteranceOutcome::Injected`].
    ///
    /// Always returns to [`SessionState::Idle`] before returning, even
    /// on error (matching Python's `finally:` that emits `status=ready`).
    pub fn stop_and_transcribe<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> Result<UtteranceOutcome, SessionError> {
        if !matches!(self.state, SessionState::Recording { .. }) {
            // Mirrors `if not self.recording: return` in Python. No
            // events, no state change.
            return Ok(UtteranceOutcome::NotRecording);
        }
        let id = match self.state {
            SessionState::Recording { id } => id,
            // Unreachable thanks to the matches! above, but pattern-
            // matching keeps the compiler honest if SessionState gains
            // a variant later.
            _ => unreachable!("guarded by matches! above"),
        };
        self.state = SessionState::Transcribing { id };

        // Audible release cue -- matches `vp_dictate.py::_stop_and_transcribe`
        // (line 704), which calls `play_cue("stop")` after capture is
        // stopped and BEFORE the transcribe pass runs. Fires exactly
        // once per utterance (guarded by the Recording -> Transcribing
        // transition above); a `NotRecording` early-return never
        // reaches this line, matching Python's `if not self.recording:
        // return` short-circuit that also skips the cue.
        self.cue_sink.play(crate::dictate::feedback::CueKind::Stop);

        // Signal the live-preview worker to stop BEFORE the final pass runs
        // so no stale `state="preview"` events land on the wire while the
        // authoritative transcribe result is being computed. Mirrors
        // `vp_dictate._stop_and_transcribe`: `if self._preview is not None:
        // self._preview.stop()` just before the final transcribe pass.
        if let Some(engine) = self.preview.as_ref() {
            engine.notify_stop();
        }

        // Drain the buffer up-front so any early-return path leaves the
        // session ready for the next press.
        let buf = std::mem::take(&mut self.frame_buf);

        // Restore audio-ducking BEFORE running transcription, matching
        // Python's `finally: self.audio_ducker.exit()` in
        // `_stop_and_transcribe` (line 706), which fires right after
        // capture stops and BEFORE the transcribe pass runs. Doing it
        // here (not after transcription) means background media returns
        // to its normal level the moment the user releases PTT, exactly
        // like the Python engine -- transcription can take seconds and
        // we don't want to keep other apps dampened that whole time.
        // `exit()` is infallible by trait contract.
        self.audio_ducker.exit();
        let outcome = self.run_transcription(writer, &buf);
        // Always settle back to Idle + emit `status=ready`, matching
        // Python's `finally: _emit_worker_event(..., state="ready")`.
        self.state = SessionState::Idle;
        wire::emit_status(writer, "ready", &self.capture_extras())?;
        outcome
    }

    /// The post-`Transcribing` branch from `_stop_and_transcribe`.
    /// Split out so the `finally`-equivalent reset + `status=ready` at
    /// the bottom of `stop_and_transcribe` cannot drift out of sync
    /// with the early-return paths.
    fn run_transcription<W: Write>(
        &mut self,
        writer: &mut W,
        buf: &[f32],
    ) -> Result<UtteranceOutcome, SessionError> {
        // Emit `transcribing` before checking for audio so every attempt has
        // the same observable state sequence.
        wire::emit_status(writer, "transcribing", &self.capture_extras())?;

        // No frames ever pushed — Python's `if not self.frames:` branch.
        if buf.is_empty() {
            wire::emit_status(writer, "no_text", &[("reason", Value::from("no_audio"))])?;
            return Ok(UtteranceOutcome::NoAudio);
        }

        // `recording_s` is reported on every no-text branch (Python:
        // `recording_s=round(recording_s, 2)` on every `_emit_worker_event`
        // call from `_stop_and_transcribe`'s no-text paths) AND on the
        // successful utterance event. Computed once up-front so every
        // branch shares the same value. Codex P2 #413 mod.rs:254 +
        // wire.rs:61 (round 2).
        let recording_s = json!(wire::round2(buf.len() as f64 / SR as f64));

        // Min-duration gate. Delegates to the existing skip helper so
        // the threshold semantics (0.3 s floor, fractional comparison)
        // stay in lock-step with `Dictate._should_skip_pcm`.
        let skip = crate::dictate::skip::should_skip(buf.len(), self.config.min_record_seconds);
        if let Some(reason) = skip.reason() {
            wire::emit_status(
                writer,
                "no_text",
                &[
                    ("reason", Value::from(reason)),
                    ("recording_s", recording_s.clone()),
                ],
            )?;
            return Ok(UtteranceOutcome::Skipped { reason });
        }

        // Transcribe, then run the dictionary replacement table FIRST --
        // BEFORE the empty / hallucination classification -- matching Python,
        // where `_dictionary_runtime(raw_text)` in `_transcribe_detail`
        // rewrites the text before `_transcribe_pcm` performs its
        // empty/hallucination checks. A replacement whose SOURCE is a blacklist
        // phrase (e.g. mapping "tak" -> "tak.") is therefore applied and the
        // CORRECTED text is (re)classified.
        let mut result = match self.transcribe.transcribe(buf, SR) {
            Err(err) => {
                // Python wraps the error and treats it as no_speech.
                wire::emit_status(
                    writer,
                    "no_text",
                    &[
                        ("reason", Value::from("no_speech")),
                        ("error", Value::from(err.to_string())),
                        ("recording_s", recording_s.clone()),
                    ],
                )?;
                return Ok(UtteranceOutcome::NoText {
                    reason: "no_speech",
                });
            }
            Ok(result) => result,
        };

        // Preserve the backend transcript before dictionary replacements for
        // the utterance event's `raw_text` field.
        let pre_dictionary_text = result.text.clone();
        let (dictated, replacements, dictionary_error) = self.apply_dictionary(&result.text);
        if let Some(error) = dictionary_error {
            wire::emit_status(writer, "dictionary_error", &[("error", Value::from(error))])?;
        }
        if dictated != result.text {
            // The dictionary rewrote the text; re-classify the corrected text
            // so a replacement can turn a blacklist phrase into normal
            // dictation (or vice versa). When nothing changed we keep the
            // backend's own `is_hallucination` verdict untouched.
            result.is_hallucination = crate::dictate::backends::is_hallucination(dictated.trim());
        }
        result.text = dictated;
        // Populate the raw_text field on the result so the wire emitter
        // has one place to read from. When the backend already surfaced
        // a raw copy (production Whisper), leave that untouched.
        if result.raw_text.is_empty() {
            result.raw_text = pre_dictionary_text;
        }

        if result.text.is_empty() {
            // Python distinguishes `too_quiet`, `no_speech`, `empty` from
            // `result.gate` so the matching UI card fires. The production gate
            // returns free-form text (e.g. "input too quiet: -42 dBFS"), so
            // route it through `normalize_gate_reason` to land on one of the
            // three reason tokens. Codex P2 #413 mod.rs:263 + mod.rs:284.
            let reason = result
                .gate
                .as_deref()
                .map(normalize_gate_reason)
                .unwrap_or("empty");
            wire::emit_status(
                writer,
                "no_text",
                &[
                    ("reason", Value::from(reason)),
                    ("recording_s", recording_s.clone()),
                ],
            )?;
            return Ok(UtteranceOutcome::NoText { reason });
        }

        if result.is_hallucination {
            wire::emit_status(
                writer,
                "no_text",
                &[
                    ("reason", Value::from("no_speech")),
                    ("recording_s", recording_s.clone()),
                ],
            )?;
            return Ok(UtteranceOutcome::NoText {
                reason: "no_speech",
            });
        }

        // Text pipeline between transcription and injection, mirroring the
        // `dictionary -> postprocess -> format -> inject` order in
        // `vp_dictate.py` / `vp_transcribe.py` (the dictionary step ran above):
        //
        // 1. LLM post-processing (optional). When a `PostProcessBackend` is
        //    attached the session emits a `post-processing` status and runs the
        //    rewrite; the production impl falls back to the input text on any
        //    provider error, so the user's dictation is never lost. `None` (the
        //    default) skips this pass AND its status entirely.
        // 2. Deterministic spoken formatting commands (`new line` -> "\n",
        //    `comma` -> ",", ...) -- a pure string transform; a `None` / `off`
        //    command set is a passthrough.
        //
        // The emitted `utterance` event carries the fully pipelined text (what
        // was actually injected) plus the `post_*` and `dictionary_replacements`
        // metadata, matching Python (`vp_dictate.py:469-475`) so
        // `ui/log_render.rs` + telemetry see what the pipeline did.
        let post = match self.post_process.as_ref() {
            // Python parity: post-processing runs when a processor is
            // configured AND the mode is not `raw`. `is_active` moves that
            // gate into the backend so a profile that flipped
            // `post_processor` mid-session is respected (Codex P1 #607),
            // while a backend that never disables itself keeps the default
            // `is_active() = true`.
            Some(backend) if backend.is_active() => {
                wire::emit_status(writer, "post-processing", &self.capture_extras())?;
                // `result.text` is the post-dictionary text (replacements
                // already applied above) and `result.language` is the
                // language the transcribe backend actually ran with for THIS
                // utterance — profile override included, or the detected one
                // on auto-detect. Both are what the cleanup prompt must be
                // built from (#686 follow-up).
                Some(backend.post_process(&result.text, &result.language))
            }
            _ => None,
        };
        let post_processed = post
            .as_ref()
            .map(|o| o.text.clone())
            .unwrap_or_else(|| result.text.clone());
        let text = crate::formatting::apply_format_commands(
            &post_processed,
            self.config.format_command_set.as_deref(),
        )
        .text;
        // Snapshot the metadata the wire emitter needs (owned copies so
        // the borrow checker does not fight the mutable state machine
        // when `inject.inject` runs in the middle of assembling the
        // event). Python parity: `_utterance_event` reads exactly the
        // same fields from `self` at event-build time.
        let dictionary_text = result.text.clone();
        let profile_name = self.active_profile.as_ref().and_then(|p| p.name.clone());
        let window = self.active_window.clone();
        wire::emit_status(writer, "injecting", &self.capture_extras())?;
        let inject_result = self.inject.inject(&text);
        let extras = wire::UtteranceExtras {
            dictionary_text: dictionary_text.as_str(),
            window: window.as_ref(),
            profile: profile_name.as_deref(),
            config: &self.config,
        };
        if let Err(err) = inject_result {
            // Python logs and continues — the utterance event still fires with
            // the text we attempted to inject. Surface the failure on the
            // utterance event so the supervisor can decide whether to retry.
            let payload = wire::emit_utterance(
                writer,
                &text,
                &result,
                recording_s.clone(),
                wire::UtterancePost {
                    inject_error: Some(err.to_string()),
                    post: post.as_ref(),
                    replacements: &replacements,
                },
                extras,
                self.command_hook_enabled(),
            )?;
            self.record_sinks(&payload);
            return Ok(UtteranceOutcome::Injected { text, result });
        }
        let payload = wire::emit_utterance(
            writer,
            &text,
            &result,
            recording_s.clone(),
            wire::UtterancePost {
                inject_error: None,
                post: post.as_ref(),
                replacements: &replacements,
            },
            extras,
            self.command_hook_enabled(),
        )?;
        self.record_sinks(&payload);
        Ok(UtteranceOutcome::Injected { text, result })
    }

    /// Hand the just-emitted utterance payload to every attached JSONL
    /// sink -- history (filtered allow-list) and metrics (full event) --
    /// in the same order Python's `_record_utterance_event` fans out to
    /// `append_record_sinks`. Missing sinks are silent no-ops (matching a
    /// session built without the corresponding `with_*_sink`). Sink
    /// errors are consumed by each implementation itself (they log a
    /// warning to stderr and return); this method has no failure mode
    /// surfaced to the state machine, so a broken sink file can never
    /// abort a dictation. Python parity: `_record_utterance_event` wraps
    /// `append_record_sinks` in `try / except OSError` and logs a warning
    /// covering BOTH sinks together.
    fn record_sinks(&self, payload: &Value) {
        if let Some(sink) = self.history_sink.as_ref() {
            sink.append(payload);
        }
        if let Some(sink) = self.metrics_sink.as_ref() {
            sink.append(payload);
        }
    }

    /// Discard the in-flight recording if `requested_epoch` matches the
    /// current recording generation.
    ///
    /// This is the chord-cancel race guard. The chord-cancel callback in
    /// `vp_keys` runs on a daemon thread that may be delayed past a
    /// release + re-press; it captures the recording generation at
    /// chord-detection time and passes it back here. Without the epoch
    /// guard a stale cancel would silently discard the NEW recording.
    /// See `vp_dictate.py:140-147 + 665-684` for the exact race.
    ///
    /// On a matching epoch:
    /// * drops the buffered frames,
    /// * settles back to [`SessionState::Idle`],
    /// * emits `status=cancelled reason=chord` then `status=ready`,
    ///   matching Python.
    ///
    /// On a stale epoch (or while idle): no-op, no events, no state
    /// change.
    pub fn cancel<W: Write>(
        &mut self,
        requested_epoch: u64,
        writer: &mut W,
    ) -> Result<(), SessionError> {
        let active_id = match self.state {
            SessionState::Recording { id } | SessionState::Opening { id } => id,
            // Idle / Transcribing: nothing to cancel. Transcribing is
            // racy in Python too — the cancel arrives after capture has
            // already stopped, so the audio is already on its way to
            // the model; matching Python we no-op.
            _ => return Ok(()),
        };
        if requested_epoch != active_id {
            // Stale cancel — the NEW recording's epoch is `active_id`,
            // not `requested_epoch`. Must NOT discard. This is the
            // load-bearing race-correctness check.
            return Ok(());
        }
        self.frame_buf.clear();
        self.state = SessionState::Idle;
        // Chord-cancel parity with `vp_dictate.py::_cancel_and_discard`
        // (lines 662-681): Python routes the cancel THROUGH
        // `_stop_and_transcribe`, which fires `play_cue("stop")` before
        // the discard branch runs. The Rust `cancel()` shortcuts around
        // `stop_and_transcribe`, so we play the cue explicitly here to
        // keep the audible "recording ended" signal even when the
        // clip is dropped.
        self.cue_sink.play(crate::dictate::feedback::CueKind::Stop);
        // Preview worker must stop (parity: _cancel_and_discard -> _stop_and_transcribe).
        if let Some(engine) = self.preview.as_ref() {
            engine.notify_stop();
        }
        // Audio ducking must exit explicitly here — Rust cancel() shortcuts
        // around stop_and_transcribe, so restore background volume manually.
        self.audio_ducker.exit();
        wire::emit_status(writer, "cancelled", &[("reason", Value::from("chord"))])?;
        wire::emit_status(writer, "ready", &self.capture_extras())?;
        Ok(())
    }

    /// The capture-backend / audio-device / capture-channels extras
    /// every status event carries. Empty strings and zero values are
    /// dropped by [`wire::emit_status`], so an unconfigured session
    /// emits a clean minimal event.
    fn capture_extras(&self) -> [(&'static str, Value); 3] {
        [
            (
                "capture_backend",
                Value::from(self.config.capture_backend.clone()),
            ),
            (
                "audio_device",
                Value::from(self.config.audio_device.clone()),
            ),
            (
                "capture_channels",
                Value::from(self.config.capture_channels),
            ),
        ]
    }
}

/// Resolve the live recording cap without depending on the feature-gated
/// `audio_route` module. The direct native pump is compiled only with audio,
/// but `DictateSession` and its tests also compile in the default feature set.
/// Parse semantics intentionally mirror `audio_route::RouteConfig::from_env`.
fn max_record_samples_from_env() -> Option<usize> {
    const DEFAULT_MAX_RECORD_S: f64 = 120.0;
    let raw = std::env::var("VOICEPI_MAX_RECORD_S").ok();
    let trimmed = raw.as_deref().map(str::trim).unwrap_or("");
    let parsed = trimmed.parse::<f64>().ok();
    if parsed == Some(0.0) {
        return None;
    }
    let seconds = parsed
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .unwrap_or(DEFAULT_MAX_RECORD_S);
    if parsed.is_some_and(|seconds| !seconds.is_finite() || seconds < 0.0) {
        crate::diag::log!(
            "[runtime] invalid VOICEPI_MAX_RECORD_S={trimmed:?}; using {DEFAULT_MAX_RECORD_S}s safety cap"
        );
    }
    Some(
        (seconds * f64::from(SR))
            .clamp(1.0, usize::MAX as f64)
            .round() as usize,
    )
}
