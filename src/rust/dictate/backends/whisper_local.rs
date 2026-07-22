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
//! The exact-blacklist [`is_hallucination`] filter now lives in the stock
//! [`super::hallucination`] module so the cloud backend shares it (matching
//! Python's backend-agnostic gate). This backend runs it after
//! [`normalize_whitespace`], exactly as before.

use std::sync::OnceLock;
use std::time::Instant;

use regex::Regex;

use super::hallucination::{is_hallucination, max_chars_per_second_from_env, speech_rate_exceeded};
use crate::dictate::session::types::{TranscribeBackend, TranscribeError, TranscribeResult};
use crate::whisper::{IdleUnloadingModel, LocalWhisper};

/// Collapse internal whitespace runs to a single space and trim both
/// ends. Mirrors Python's
/// `re.sub(r"\s+", " ", "".join(s.text for s in segment_list)).strip()`
/// in `vp_transcribe.py::_transcribe_detail` — segments returned by
/// whisper.cpp carry leading spaces on word boundaries, and a naive
/// concatenation leaves runs of whitespace + leading/trailing slack
/// that would (a) defeat the exact-match hallucination blacklist for
/// strings like `" tak"` and (b) inject visible extra spaces.
/// Codex P2 #417 whisper_local.rs:201.
fn normalize_whitespace(text: &str) -> String {
    static WS_RUN: OnceLock<Regex> = OnceLock::new();
    let re = WS_RUN.get_or_init(|| Regex::new(r"\s+").expect("whitespace regex is valid"));
    re.replace_all(text.trim(), " ").into_owned()
}

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
/// - `is_hallucination` — [`is_hallucination`] match against the text.
/// - `latency_ms` — wall-clock time spent in [`IdleUnloadingModel::with_model`]
///   (covers a lazy reload too, matching the Python `compute_s` field).
/// - `duration_s` — `pcm.len() / sample_rate`, the captured audio length.
/// - `language` — the configured hint (or empty for auto); whisper-rs
///   does not currently surface a detected-language code through
///   [`LocalWhisper::transcribe_samples`].
/// - `gate` — `Some(reason)` when the pre-transcription speech gate
///   (`vp_transcribe._looks_like_speech` parity, via
///   [`crate::audio_dsp::speech_gate_reason`]) rejects too-quiet /
///   no-contrast audio BEFORE the model loads; `None` on a normal pass.
///   The session maps the reason to a `too_quiet`/`no_speech` no-text
///   event via `crate::dictate::session::normalize_gate_reason`.
pub struct WhisperLocalTranscribeBackend {
    model: IdleUnloadingModel<LocalWhisper>,
    config: WhisperBackendConfig,
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
        Self { model, config }
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
        let initial_prompt = self
            .config
            .initial_prompt
            .as_deref()
            .filter(|s| !s.is_empty());

        // `duration_s` from sample count + rate (Python computes the same
        // way once the PCM is in hand). Guard `sample_rate = 0` so the f64
        // division does not produce `inf`/`NaN` that JSON would reject.
        let duration_s = if sample_rate == 0 {
            0.0
        } else {
            pcm.len() as f64 / f64::from(sample_rate)
        };

        // Pre-transcription speech gate, matching Python's
        // `vp_transcribe._transcribe_detail`: reject too-quiet /
        // no-contrast audio BEFORE loading/decoding with whisper.cpp. The
        // reason string flows onto `TranscribeResult.gate`, which the
        // session maps to a `too_quiet`/`no_speech` no-text event.
        if let Some(reason) =
            crate::audio_dsp::speech_gate_reason(pcm, &crate::audio_dsp::thresholds_from_env())
        {
            return Ok(TranscribeResult {
                text: String::new(),
                gate: Some(reason),
                duration_s,
                ..Default::default()
            });
        }

        let start = Instant::now();
        let raw_text = self
            .model
            .with_model(|m| m.transcribe_samples(pcm, language_hint, initial_prompt))
            .map_err(|e| TranscribeError::Backend(format!("{e:#}")))?;
        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        // Collapse whitespace runs + trim both ends BEFORE the
        // blacklist check and BEFORE injection, matching Python's
        // `re.sub(r"\s+", " ", ...).strip()` in `_transcribe_detail`.
        // Without this a quiet-audio hallucination like `" tak"` would
        // bypass the exact-match filter (which only trims the right
        // end), and a normal segment with the typical leading
        // whisper.cpp word-boundary space would be injected verbatim.
        // Codex P2 #417 whisper_local.rs:201.
        let text = normalize_whitespace(&raw_text);

        // Impossible-speech-rate hallucination guard (Python's
        // `_exceeds_speech_rate`): a transcript produced far faster than
        // real speech is blanked, so it surfaces as an `empty` no-text
        // event rather than injecting a hallucinated wall of caption text.
        let text = if speech_rate_exceeded(&text, duration_s, max_chars_per_second_from_env()) {
            String::new()
        } else {
            text
        };

        // Compute the hallucination flag against a borrow so `text` can
        // be moved into the result without a redundant clone.
        let is_hallucination = is_hallucination(&text);
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
