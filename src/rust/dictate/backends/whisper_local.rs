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
//! # Hallucination filter — partial port
//!
//! The Python `is_hallucination` helper in
//! `src/python/whisper_dictate/vp_transcribe.py` checks two things:
//!
//! 1. **Exact blacklist match** — the lowercased / rstripped text is in
//!    `HALLUCINATIONS` (data: `whisper_dictate/data/hallucination_patterns.json::exact_blacklist`).
//! 2. **Credit regex match** — the whole text matches one of the
//!    subtitle-credit patterns assembled by `_build_credit_re`.
//!
//! Only (1) is ported here. The credit-regex port is deferred to a Wave 5
//! PR 5 follow-up because porting the regex assembler + the JSON loader
//! is its own multi-file change. The blacklist is the most common path
//! in practice (every observed false positive on quiet Danish input has
//! been an exact `"tak"`-family match) and the session's empty-text
//! branch catches credit-style hallucinations through the
//! `_exceeds_speech_rate` guard once that is wired.
//!
//! The blacklist literals are kept in sync with
//! `whisper_dictate/data/hallucination_patterns.json::exact_blacklist`
//! by copying verbatim — see [`EXACT_BLACKLIST`].

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Instant;

use regex::Regex;

use crate::dictate::session::types::{TranscribeBackend, TranscribeError, TranscribeResult};
use crate::whisper::{IdleUnloadingModel, LocalWhisper};

/// Exact-match hallucination blacklist, ported verbatim from
/// `src/python/whisper_dictate/data/hallucination_patterns.json::exact_blacklist`.
///
/// Whisper emits one of these strings on quiet / empty audio — they are
/// subtitle / caption credits the multilingual model picked up from its
/// training set. Matched against `text.to_lowercase().trim_end()` to
/// mirror Python's `text.lower().rstrip()`.
///
/// MUST stay byte-identical to the JSON data file. When the JSON file
/// gains a new entry the same entry must be added here (and a regression
/// test should catch the drift — see [`is_hallucination`]'s tests).
pub(crate) const EXACT_BLACKLIST: &[&str] = &[
    "tak",
    "tak.",
    "tak for din opmærksomhed",
    "tak for din opmærksomhed.",
    "tak fordi du så med",
    "tak fordi du så med.",
    "tak fordi du lyttede med",
    "tak fordi du lyttede med.",
    "tak for at du så med",
    "tak for at du så med.",
    "tak for at i så med",
    "tak for at i så med.",
    "tak fordi i så med",
    "tak fordi i så med.",
    "thank you",
    "thank you.",
    "thank you for watching",
    "thank you for watching.",
    "thank you for listening",
    "thank you for listening.",
    "thanks for watching",
    "thanks for watching.",
    "undertekster af",
    "undertekstet af",
];

/// `true` iff `text` is on the exact-match hallucination blacklist.
///
/// Partial port of Python's `vp_transcribe.is_hallucination` — only the
/// `text.lower().rstrip() in HALLUCINATIONS` branch is implemented. See
/// the module docs for the credit-regex deferral note.
pub fn is_hallucination(text: &str) -> bool {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    let set = SET.get_or_init(|| EXACT_BLACKLIST.iter().copied().collect());
    let lowered = text.to_lowercase();
    set.contains(lowered.trim_end())
}

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
/// - `gate` — always `None`. The speech-gate lives in
///   `vp_transcribe._looks_like_speech` and is its own Wave 5 follow-up
///   (the gate text-to-reason mapper already exists in
///   `crate::dictate::session::normalize_gate_reason`, ready to consume
///   `Some(raw_gate_text)` when a future backend / pre-processing step
///   produces one).
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

        // `duration_s` from sample count + rate (Python computes the
        // same way once the PCM is in hand — see `vp_transcribe.py::
        // _transcribe_detail`'s `dur = len(audio) / SR`). Guard against
        // a caller passing `sample_rate = 0` so the f64 division does
        // not produce `inf` / `NaN` that downstream JSON serialisation
        // would reject.
        let duration_s = if sample_rate == 0 {
            0.0
        } else {
            pcm.len() as f64 / f64::from(sample_rate)
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
