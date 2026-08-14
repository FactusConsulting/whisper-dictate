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

use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::hallucination::{finalize_transcript, max_chars_per_second_from_env};
use crate::dictate::session::preview::{PreviewBackend, PreviewError};
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
    /// Model instance wrapped in `Arc<>` so a live-preview engine can share
    /// the same resident model without doubling RAM (see
    /// [`Self::share_for_preview`] and
    /// [`crate::dictate::session::preview::PreviewEngine`]). Both the final
    /// transcribe pass here and the preview thread serialise on the
    /// wrapper's internal `Mutex<Option<M>>`, matching Python's
    /// `TRANSCRIBE_LOCK` semantics (a preview never runs while the final
    /// pass holds the lock and vice versa).
    model: Arc<IdleUnloadingModel<LocalWhisper>>,
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
    /// Per-utterance target-profile overrides. Populated by
    /// [`TranscribeBackend::apply_profile_overrides`] before every
    /// `transcribe`; `initial_prompt` short-circuits the reload-prompt
    /// fold and `language` overrides the config hint for a single
    /// utterance. Reset to `None` when the profile does not match
    /// (empty settings map). Codex P1 #607.
    profile_prompt: Mutex<Option<String>>,
    profile_language: Mutex<Option<String>>,
    /// Last `model` override value the profile system reported. Used to
    /// dedupe the "model change deferred" stderr warning so a user with
    /// a profile that pins a model to a specific whisper file only sees
    /// the warning once per profile match, not once per utterance. `None`
    /// means we have not seen a model override yet. Deferred: the resident
    /// [`IdleUnloadingModel`] cannot swap its GGML file mid-session, so
    /// the override is skipped with a warning event and requires a
    /// supervisor restart to take effect (matches Python's
    /// `_report_restart_required` flow for `model` in
    /// `_apply_effective_config`).
    profile_model_warned: Mutex<Option<String>>,
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
            model: Arc::new(model),
            config,
            prompt_reload: None,
            profile_prompt: Mutex::new(None),
            profile_language: Mutex::new(None),
            profile_model_warned: Mutex::new(None),
        }
    }

    /// Return a [`PreviewBackend`] wrapper that shares this backend's
    /// resident model instance -- so a live-preview worker
    /// ([`crate::dictate::session::preview::PreviewEngine`]) can run cheap
    /// mid-utterance transcribes without loading a second copy of the
    /// GGML weights into RAM. Both this backend's `transcribe` and the
    /// preview's `transcribe_partial` serialise on the wrapper's internal
    /// `Mutex<Option<M>>` -- so a preview can never run concurrently with
    /// the final pass (mirroring Python's `TRANSCRIBE_LOCK`).
    ///
    /// The returned wrapper's `Send + Sync` bound is satisfied by
    /// [`Arc<IdleUnloadingModel<LocalWhisper>>`] (both `Send + Sync`) so
    /// it can move into the preview worker thread.
    pub fn share_for_preview(&self) -> WhisperLocalPreviewBackend {
        WhisperLocalPreviewBackend {
            model: Arc::clone(&self.model),
            language: self.config.language.clone().filter(|s| !s.is_empty()),
        }
    }

    /// The language hint that will apply to the NEXT utterance: profile
    /// override wins over the config hint. `Some("")` is treated as
    /// "auto detect" and normalised to `None` here so the whisper.cpp
    /// loader is never handed an empty string. Codex P1 #607.
    fn effective_language(&self) -> Option<String> {
        let profile = self
            .profile_language
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        profile
            .or_else(|| self.config.language.clone())
            .filter(|s| !s.is_empty())
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

    /// Attach a dictionary prompt provider owned by the in-process runtime.
    #[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
    pub(crate) fn with_reloading_prompt_settings(
        mut self,
        settings: crate::dictionary::RuntimeDictionarySettings,
    ) -> Self {
        self.prompt_reload = Some(Box::new(Mutex::new(
            crate::dictionary::ReloadingDictionary::from_settings(settings),
        )));
        self
    }

    /// The effective STT prompt for this utterance. Order of precedence
    /// (highest first):
    ///
    /// 1. **Profile override** (`initial_prompt` key on the matched
    ///    profile) — populated by
    ///    [`TranscribeBackend::apply_profile_overrides`]. This wins over
    ///    every other source so a per-app profile can pin a specific
    ///    vocabulary hint for one utterance (Codex P1 #607).
    /// 2. **Reload-prompt fold** — `config.initial_prompt` treated as the
    ///    BASE + the live dictionary terms, re-read each utterance under
    ///    [`crate::dictionary::ReloadingDictionary`].
    /// 3. **Fixed config prompt** — `config.initial_prompt` verbatim when
    ///    no reloading prompt is attached.
    fn effective_prompt(&self) -> (Option<String>, Vec<String>) {
        if let Some(profile) = self
            .profile_prompt
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return (Some(profile), Vec::new());
        }
        match &self.prompt_reload {
            Some(reload) => reload
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .initial_prompt_with_terms(self.config.initial_prompt.as_deref()),
            None => (self.config.initial_prompt.clone(), Vec::new()),
        }
    }

    /// Read-only access to the wrapped idle-unloading model. Exposed so
    /// the supervisor (UI / telemetry) can observe `is_loaded()` /
    /// `idle_timeout()` without an extra channel.
    pub fn model(&self) -> &IdleUnloadingModel<LocalWhisper> {
        self.model.as_ref()
    }

    /// Expose the shared model handle so a caller (e.g. the runtime factory)
    /// can wire ancillary consumers (preview, telemetry) that need to share
    /// the same resident model instance. Prefer [`Self::share_for_preview`]
    /// when the consumer is the live-preview engine.
    pub fn shared_model(&self) -> Arc<IdleUnloadingModel<LocalWhisper>> {
        Arc::clone(&self.model)
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
        //
        // Profile-override precedence: `effective_language` /
        // `effective_prompt` consult the profile slot first so a per-app
        // profile's `language` / `initial_prompt` keys land on the model
        // for THIS utterance (Codex P1 #607).
        let effective_language = self.effective_language();
        let language_hint = effective_language.as_deref();
        // Re-fold the dictionary terms into the prompt per utterance when a
        // reloading prompt is attached (else the fixed config prompt).
        let (folded_prompt, dictionary_terms) = self.effective_prompt();
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
            // Preserve the untouched decoded text as `raw_text` so the
            // utterance event carries it verbatim, matching Python's
            // `TranscribeResult(raw_text=raw_text, text=text, ...)`
            // shape. The session falls back to `dictionary_text` when
            // this is empty; leaving it set here means the local
            // backend's row shape matches Python 1:1. Codex P1 #606.
            raw_text: raw_text.clone(),
            dictionary_terms: (!dictionary_terms.is_empty())
                .then(|| dictionary_terms.into_boxed_slice()),
            text,
            is_hallucination,
            latency_ms,
            duration_s,
            // Mirror the ACTUAL hint we passed to whisper.cpp so the
            // utterance event reflects a profile-driven override rather
            // than the stale construction-time config value (Codex P1
            // #607). Empty when auto-detect ran.
            language: effective_language.unwrap_or_default(),
            gate: None,
            // Provenance, resolved AFTER the `with_model` call above so a
            // lazy (or post-idle-unload re-)load has already produced
            // whisper.cpp's `whisper_backend_init_gpu: ...` verdict. Read
            // from `whisper::accel`, i.e. from what whisper.cpp reported,
            // NOT from `VOICEPI_WHISPER_GPU` or the compiled-in Vulkan
            // feature -- a Vulkan-linked binary on a box with no usable
            // driver falls back to CPU silently and that has to be visible
            // on the record.
            stt_impl: crate::dictate::provenance::STT_IMPL_WHISPER_CPP.to_owned(),
            stt_accel: crate::whisper::accel::resolved_label().to_owned(),
            // Local Whisper does not currently expose a language
            // probability (the whisper.cpp binding used here surfaces
            // detected-language token IDs only); the payload's
            // `language_probability` field is dropped when 0.0 so
            // downstream tooling sees no field rather than a false 0.
            ..Default::default()
        })
    }

    fn apply_profile_overrides(&self, settings: &std::collections::BTreeMap<String, String>) {
        if let Some(reload) = self.prompt_reload.as_ref() {
            crate::dictionary::DictionaryProvider::apply_settings(
                &mut *reload
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
                settings,
            );
        }
        // `initial_prompt`: profile wins over the reload-prompt fold + config
        // (see `effective_prompt`). Blank / whitespace-only string is treated
        // as "reset the override" -- matching the settings-schema treatment
        // of an empty value as "unset".
        let prompt_override = settings
            .get("initial_prompt")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        *self
            .profile_prompt
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = prompt_override;
        // `language`: same treatment. `effective_language` collapses a
        // blank string to `None` so the whisper.cpp loader never sees a
        // literal empty language code.
        let language_override = settings
            .get("language")
            .or_else(|| settings.get("lang"))
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        *self
            .profile_language
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = language_override;
        // `model`: DEFERRED. Swapping the GGML file mid-session is
        // non-trivial (the resident `IdleUnloadingModel` owns its file
        // path + memory-mapped weights; a hot-swap would need coordination
        // with the preview worker + a graceful unload of the current
        // model). Mirrors Python's `_report_restart_required` which prints
        // a one-shot warning for model changes in `_apply_effective_config`.
        // Filed as follow-up in the PR body.
        if let Some(model) = settings
            .get("model")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
        {
            let mut warned = self
                .profile_model_warned
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if warned.as_deref() != Some(model) {
                crate::diag::log!(
                    "[profile] model_change_deferred model={} restart_needed=true \
                     (the resident whisper.cpp model cannot swap mid-session; \
                     restart the app for a `model` profile override to take effect)",
                    model
                );
                *warned = Some(model.to_owned());
            }
        } else {
            // The profile did not request a specific model -- clear the
            // dedupe slot so a later profile that re-introduces `model=X`
            // re-warns exactly once (rather than being permanently muted
            // by the first warning).
            *self
                .profile_model_warned
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = None;
        }
    }
}

/// [`PreviewBackend`] wrapper around a shared
/// [`Arc<IdleUnloadingModel<LocalWhisper>>`] -- constructed via
/// [`WhisperLocalTranscribeBackend::share_for_preview`]. Runs the same
/// pre-transcription speech gate the final pass uses (too-quiet audio ->
/// empty string, no model load); on a passing buffer it invokes the
/// shared model and returns the decoded text as-is. Skips the
/// hallucination filter and dictionary rewrite entirely -- previews are
/// display-only so the raw model text is what the UI should show growing.
pub struct WhisperLocalPreviewBackend {
    model: Arc<IdleUnloadingModel<LocalWhisper>>,
    /// Language hint, already collapsed from `Some("")` -> `None` so the
    /// whisper.cpp loader is never handed a literal empty string
    /// (matches [`WhisperLocalTranscribeBackend::transcribe`]'s guard).
    language: Option<String>,
}

impl PreviewBackend for WhisperLocalPreviewBackend {
    fn transcribe_partial(&self, pcm: &[f32], sample_rate: u32) -> Result<String, PreviewError> {
        // Pre-transcription speech gate: reject too-quiet / no-contrast
        // audio BEFORE loading the model. On rejection return "" so the
        // preview engine skips the emission (matching the empty-text
        // branch in `run_tick`); no error surfaces because a gated
        // preview is not a failure.
        let audio = match crate::audio_dsp::prepare_for_transcription(
            pcm,
            sample_rate,
            &crate::audio_dsp::thresholds_from_env(),
        ) {
            crate::audio_dsp::PreparedAudio::Reject { .. } => return Ok(String::new()),
            crate::audio_dsp::PreparedAudio::Decode { audio, .. } => audio,
        };
        // Pass `None` as `initial_prompt` -- the preview is a rolling
        // window (mostly the recent tail) so dictionary-biased hints are
        // less useful than for the final pass; keep this fast + simple.
        self.model
            .with_model(|m| m.transcribe_samples(&audio, self.language.as_deref(), None))
            .map_err(|e| PreviewError::Backend(format!("{e:#}")))
    }
}

#[cfg(test)]
#[path = "whisper_local_tests.rs"]
mod tests;
