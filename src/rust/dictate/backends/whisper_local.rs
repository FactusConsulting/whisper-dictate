//! [`TranscribeBackend`] impl that wraps the local whisper.cpp model.
//!
//! Gated on the `whisper-rs-local` cargo feature so default builds never
//! pull whisper-rs / CMake into the dep graph. Wraps
//! [`IdleUnloadingModel<LocalWhisper>`] (the Wave 7-A primitive) rather
//! than [`LocalWhisper`] directly so the production wiring inherits the
//! idle-unload behaviour for free — a long-running supervisor session
//! drops the model after `VOICEPI_WHISPER_IDLE_UNLOAD_S` of inactivity
//! and lazy-reloads on the next press.
//!
//! Wave 5 PR 5-prep: no production caller in this PR — the
//! coordinator-sink wiring (PR 4) continues to use the stub backend
//! until PR 5 swaps it for this one.
//!
//! # Hallucination filter
//!
//! The whole-text finalization — whitespace normalize, impossible-speech-rate
//! blanking, and the exact-blacklist / credit-regex hallucination gate — lives
//! in the stock [`super::hallucination`] module (`finalize_transcript`) so the
//! cloud backend shares it and it is unit-tested on every build (matching
//! Python's backend-agnostic gate). This backend calls it after decoding.

use std::sync::Mutex;
use std::time::Instant;

use super::hallucination::{finalize_transcript, max_chars_per_second_from_env};
use crate::dictate::session::types::{TranscribeBackend, TranscribeError, TranscribeResult};
use crate::whisper::{IdleUnloadingModel, LocalWhisper};

/// Per-call language + initial-prompt hints fed to whisper.cpp on every
/// transcribe pass. Mirrors the Python wiring layer's plumbing
/// (`vp_transcribe.py::_transcribe_detail` reads `lang` and an upstream
/// dictionary-derived prompt). Kept as `Option<String>` so the caller
/// can plumb config that may be unset; both `None` and `Some("")` are
/// treated as "no hint" by [`LocalWhisper::transcribe_samples`].
#[derive(Debug, Clone, Default)]
pub struct WhisperBackendConfig {
    /// BCP-47-ish language hint passed to whisper.cpp. `None` /
    /// `Some("auto")` lets whisper.cpp auto-detect (multilingual
    /// models only). The detected/forced code is mirrored back into
    /// [`TranscribeResult::language`] so the session's worker-event
    /// stream stays byte-equivalent to Python's.
    pub language: Option<String>,
    /// Optional dictionary-derived initial prompt, biasing whisper.cpp's
    /// decoder toward rare-word recognition. Empty `Some("")` is
    /// treated as `None` by [`LocalWhisper::transcribe_samples`].
    pub initial_prompt: Option<String>,
}

/// Production [`TranscribeBackend`] wrapping [`IdleUnloadingModel<LocalWhisper>`].
///
/// Construction is cheap — the wrapped [`IdleUnloadingModel`] does not
/// load the model until the first [`Self::transcribe`] call. Subsequent
/// calls reuse the resident model until the idle watcher unloads it,
/// after which the next call lazy-reloads.
///
/// The session-level [`TranscribeResult`] fields are populated as
/// follows on a successful pass:
///
/// - `text`     — whisper.cpp's decoded text.
/// - `is_hallucination` — [`super::hallucination::is_hallucination`] match
///   against the finalized text.
/// - `latency_ms` — wall-clock time spent in [`IdleUnloadingModel::with_model`]
///   (covers a lazy reload too, matching the Python `compute_s` field).
/// - `duration_s` — `trimmed.len() / sample_rate`: the captured audio
///   length AFTER the trailing dead-air tail is trimmed, so it matches the
///   buffer actually decoded (Python's `dur`). The gain boost applied
///   before decode is level-only and does not change it.
/// - `language` — the configured hint (or empty for auto); whisper-rs
///   does not currently surface a detected-language code through
///   [`LocalWhisper::transcribe_samples`].
/// - `gate` — `Some(reason)` when the pre-transcription speech gate
///   (`vp_transcribe._looks_like_speech` parity, via
///   [`crate::audio_dsp::prepare_for_transcription`]) rejects too-quiet /
///   no-contrast audio BEFORE the model loads; `None` on a normal pass.
///   The session maps the reason to a `too_quiet`/`no_speech` no-text
///   event via `crate::dictate::session::normalize_gate_reason`.
pub struct WhisperLocalTranscribeBackend {
    model: IdleUnloadingModel<LocalWhisper>,
    config: WhisperBackendConfig,
    /// When set, the STT prompt is re-folded from `config.initial_prompt`
    /// (treated as the BASE prompt) + the live dictionary terms on every
    /// `transcribe`, so dictionary term / budget edits re-bias whisper.cpp
    /// without an app restart (Python's per-utterance
    /// `_dictionary_prompt_runtime`). `None` keeps the fixed
    /// `config.initial_prompt`. `Mutex` because the reload cache mutates behind
    /// `transcribe(&self)`; boxed to keep the backend small when no reloading
    /// prompt is attached.
    prompt_reload: Option<Box<Mutex<crate::dictionary::ReloadingDictionary>>>,
}

impl WhisperLocalTranscribeBackend {
    /// Build a backend around an already-constructed idle-unloading
    /// model wrapper.
    ///
    /// Take the [`IdleUnloadingModel`] by value so the backend owns the
    /// reload policy and the watcher-thread lifetime end-to-end. The
    /// caller is expected to construct the wrapper via
    /// [`IdleUnloadingModel::for_local_whisper`] with the user-resolved
    /// model path + idle timeout (parsed from
    /// `VOICEPI_WHISPER_IDLE_UNLOAD_S` via
    /// [`crate::whisper::parse_idle_timeout_from_env`]).
    pub fn new(model: IdleUnloadingModel<LocalWhisper>, config: WhisperBackendConfig) -> Self {
        Self {
            model,
            config,
            prompt_reload: None,
        }
    }

    /// Attach a live-reloading STT prompt: `config.initial_prompt` is treated as
    /// the BASE prompt and the dictionary terms are re-folded into it on each
    /// `transcribe`, under `precedence` (`ConfigFirst` for the live worker). The
    /// caller must NOT pre-fold the terms into `config.initial_prompt` -- pass
    /// the raw `VOICEPI_INITIAL_PROMPT` base so the terms can be re-folded live.
    pub fn with_reloading_prompt(
        mut self,
        precedence: crate::dictionary::ReloadPrecedence,
    ) -> Self {
        self.prompt_reload = Some(Box::new(Mutex::new(
            crate::dictionary::ReloadingDictionary::new(precedence),
        )));
        self
    }

    /// The effective STT prompt for this utterance: the live-reloaded
    /// base + terms when a reloading prompt is attached, else the fixed
    /// `config.initial_prompt`.
    fn effective_prompt(&self) -> Option<String> {
        match &self.prompt_reload {
            Some(reload) => reload
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .initial_prompt(self.config.initial_prompt.as_deref()),
            None => self.config.initial_prompt.clone(),
        }
    }

    /// Read-only access to the wrapped idle-unloading model. Exposed so
    /// the supervisor (UI / telemetry) can observe `is_loaded()` /
    /// `idle_timeout()` without an extra channel.
    pub fn model(&self) -> &IdleUnloadingModel<LocalWhisper> {
        &self.model
    }

    /// Configured per-call hints.
    pub fn config(&self) -> &WhisperBackendConfig {
        &self.config
    }
}

impl TranscribeBackend for WhisperLocalTranscribeBackend {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        // Normalize the language hint up-front: an empty string from
        // the settings layer must collapse to `None` so
        // `LocalWhisper::transcribe_samples` triggers auto-detect.
        // Without this an empty `Some("")` from the default config
        // would be forwarded as a literal language code, which the
        // whisper.cpp loader rejects with a cryptic error on the
        // first real transcription. Same treatment for the prompt so
        // the contract documented on `WhisperBackendConfig` actually
        // holds. Codex P2 #417 whisper_local.rs:183.
        let language_hint = self.config.language.as_deref().filter(|s| !s.is_empty());
        // Re-fold the dictionary terms into the prompt per utterance when a
        // reloading prompt is attached (else the fixed config prompt).
        let folded_prompt = self.effective_prompt();
        let initial_prompt = folded_prompt.as_deref().filter(|s| !s.is_empty());

        // Full pre-model pipeline of Python's `vp_transcribe._transcribe_detail`
        // (`vp_transcribe.py:1255-1267`): trim the trailing dead-air tail ONCE,
        // gate the trimmed buffer (reject too-quiet / no-contrast audio BEFORE
        // loading/decoding with whisper.cpp), and boost the quiet body toward
        // the target level. `duration_s` comes from the trimmed length; the
        // gate reason flows onto `TranscribeResult.gate`, which the session
        // maps to a `too_quiet`/`no_speech` no-text event.
        let (audio, duration_s) = match crate::audio_dsp::prepare_for_transcription(
            pcm,
            sample_rate,
            &crate::audio_dsp::thresholds_from_env(),
        ) {
            crate::audio_dsp::PreparedAudio::Reject { reason, duration_s } => {
                return Ok(TranscribeResult {
                    text: String::new(),
                    gate: Some(reason),
                    duration_s,
                    ..Default::default()
                });
            }
            crate::audio_dsp::PreparedAudio::Decode { audio, duration_s } => (audio, duration_s),
        };

        let start = Instant::now();
        let raw_text = self
            .model
            .with_model(|m| m.transcribe_samples(&audio, language_hint, initial_prompt))
            .map_err(|e| TranscribeError::Backend(format!("{e:#}")))?;
        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        // Collapse whitespace, blank impossibly-fast transcripts, and flag
        // exact-blacklist hallucinations -- the pure tail of Python's
        // `_transcribe_detail`, factored out so it is unit-testable without a
        // whisper.cpp model (see `finalize_transcript`).
        let (text, is_hallucination) =
            finalize_transcript(&raw_text, duration_s, max_chars_per_second_from_env());
        Ok(TranscribeResult {
            text,
            is_hallucination,
            latency_ms,
            duration_s,
            language: self.config.language.clone().unwrap_or_default(),
            gate: None,
        })
    }
}

#[cfg(test)]
#[path = "whisper_local_tests.rs"]
mod tests;
