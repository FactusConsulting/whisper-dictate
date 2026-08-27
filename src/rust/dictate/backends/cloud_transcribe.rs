//! Cloud (OpenAI-compatible / Groq) [`TranscribeBackend`] for the
//! in-process Rust dictation session.
//!
//! The in-process engine only ran local Whisper; the Python worker also
//! supports `stt_backend=openai` (a cloud `/audio/transcriptions`
//! endpoint). This backend closes that parity gap: it encodes the
//! captured 16 kHz mono PCM to an in-memory WAV and POSTs it via
//! [`crate::cloud_api::cloud_transcribe`], so a `DictateSession` can run
//! cloud STT with **no local model, GPU, or Python** -- reading the same
//! `VOICEPI_STT_*` settings the worker command exports.
//!
//! Stock (no cargo feature): `cloud_api` (ureq) + `hound` (WAV) are both
//! unconditional deps, so this compiles and is unit-tested on every build.
//! The feature-gated `make_real_session` selection (cloud vs local) is a
//! separate step; this module is the reusable, testable primitive.

use std::io::Cursor;
use std::sync::Mutex;
use std::time::Instant;

#[cfg(test)]
use super::hallucination::max_chars_per_second_from_env;
use super::hallucination::{is_hallucination, speech_rate_exceeded, TranscriptionGuards};
use crate::cloud_api::{
    cloud_transcribe, cloud_transcribe_for_provider, CloudTranscriptionRequest,
    CloudTranscriptionResult,
};
use crate::dictate::{TranscribeBackend, TranscribeError, TranscribeResult};

/// `settings_schema.json` env keys for the cloud STT backend.
pub const STT_BACKEND_ENV: &str = "VOICEPI_STT_BACKEND";
pub const STT_BASE_URL_ENV: &str = "VOICEPI_STT_BASE_URL";
pub const STT_MODEL_ENV: &str = "VOICEPI_STT_MODEL";
pub const STT_TIMEOUT_MS_ENV: &str = "VOICEPI_STT_TIMEOUT_MS";
/// Spoken-language + initial-prompt hints, shared with the local backend
/// (`vp_cli.py` reads the same vars).
pub const LANG_ENV: &str = "VOICEPI_LANG";
pub const INITIAL_PROMPT_ENV: &str = "VOICEPI_INITIAL_PROMPT";

/// `stt_backend` value that selects this cloud backend.
pub const STT_BACKEND_CLOUD: &str = "openai";

const DEFAULT_STT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_STT_TIMEOUT_MS: u64 = 30_000;
const STT_TIMEOUT_MIN_MS: u64 = 100;
pub const NEMOTRON_MODEL: &str = "nvidia/nemotron-3.5-asr-streaming-0.6b";

pub fn is_nemotron_config(config: &CloudTranscribeConfig) -> bool {
    config.model.trim().eq_ignore_ascii_case(NEMOTRON_MODEL)
}

/// Resolved cloud-STT settings. Mirrors the fields
/// [`crate::cloud_api::cloud_transcribe`] consumes.
#[derive(Debug, Clone)]
pub struct CloudTranscribeConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_ms: u64,
    pub language: Option<String>,
    pub prompt: Option<String>,
}

impl CloudTranscribeConfig {
    /// Build from the process environment (the `VOICEPI_STT_*` vars the UI
    /// exports into the worker command). Convenience wrapper around
    /// [`Self::from_env_with`].
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// Testable core: resolves every field through `lookup` so the parse is
    /// unit-tested without touching process env. The API key follows the
    /// same precedence as `ui/api_keys.rs::load_stt_api_key_from_env`:
    /// the STT-specific key first, then ONLY the generic var for the
    /// provider implied by `base_url` (groq vs openai).
    pub fn from_env_with(lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self::from_env_with_provider(lookup, "")
    }

    pub fn from_env_with_provider(lookup: impl Fn(&str) -> Option<String>, provider: &str) -> Self {
        let get = |name: &str| {
            lookup(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let base_url = get(STT_BASE_URL_ENV).unwrap_or_else(|| DEFAULT_STT_BASE_URL.to_owned());
        let model = get(STT_MODEL_ENV).unwrap_or_default();
        let generic_key_env = if base_url.to_ascii_lowercase().contains("groq.com") {
            "GROQ_API_KEY"
        } else {
            "OPENAI_API_KEY"
        };
        let api_key = get("VOICEPI_STT_API_KEY")
            .or_else(|| get(generic_key_env))
            .unwrap_or_default();
        let api_key = if provider.trim().eq_ignore_ascii_case("nemotron")
            || model.eq_ignore_ascii_case(NEMOTRON_MODEL)
        {
            get("VOICEPI_STT_API_KEY").unwrap_or_default()
        } else {
            api_key
        };
        let timeout_ms = get(STT_TIMEOUT_MS_ENV)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .map(|v| (v.trunc().max(0.0) as u64).max(STT_TIMEOUT_MIN_MS))
            .unwrap_or(DEFAULT_STT_TIMEOUT_MS);
        Self {
            base_url,
            api_key,
            model,
            timeout_ms,
            language: get(LANG_ENV),
            prompt: get(INITIAL_PROMPT_ENV),
        }
    }
}

/// True when the operator selected the cloud STT backend
/// (`VOICEPI_STT_BACKEND=openai`). Case-insensitive; unset / any other
/// value resolves to false (local Whisper).
pub fn cloud_backend_requested_from_env() -> bool {
    std::env::var(STT_BACKEND_ENV)
        .map(|v| v.trim().eq_ignore_ascii_case(STT_BACKEND_CLOUD))
        .unwrap_or(false)
}

/// Build a [`CloudTranscribeBackend`] from `config`, enforcing the
/// local-only privacy lock FIRST.
///
/// Mirrors the Python worker's `_assert_local_backend` gate in
/// `vp_transcribe.py::load_stt_model`: under `local_only`, a remote
/// (`openai`/Groq) STT backend is refused so microphone audio never
/// leaves the machine -- EXCEPT when the configured `base_url` is a
/// loopback endpoint (a self-hosted server on `localhost`/`127.0.0.1`
/// never leaves the box), which stays allowed. The Rust in-process
/// session previously had no such check, so `VOICEPI_LOCAL_ONLY=1` +
/// `stt_backend=openai` would still POST audio remotely.
///
/// Returns the human-readable rejection message on a blocked backend so
/// [`crate::runtime::rust_session_real_backends::make_real_session`] can
/// surface it on the runtime event channel and fall back to the stub
/// session (never silently dictating to a remote endpoint). `local_only`
/// is passed in (rather than read here) so the gate is unit-testable
/// without touching process env / `settings.json`; the production caller
/// supplies [`crate::whisper::model_manager::is_local_only`].
pub fn cloud_backend_local_only_checked(
    local_only: bool,
    config: CloudTranscribeConfig,
) -> Result<CloudTranscribeBackend, String> {
    cloud_backend_local_only_checked_with_provider(local_only, config, "")
}

/// Build a cloud backend while preserving the caller's selected provider.
///
/// The provider is deliberately carried separately from the model: a custom
/// OpenAI-compatible server may use the Nemotron model id without speaking
/// Riva gRPC, and routing based on the model alone would send it protobuf
/// traffic. An empty provider keeps the legacy model-alias inference used by
/// older CLI callers that do not have a provider setting available.
pub fn cloud_backend_local_only_checked_with_provider(
    local_only: bool,
    config: CloudTranscribeConfig,
    provider: &str,
) -> Result<CloudTranscribeBackend, String> {
    crate::privacy::assert_local_backend(
        local_only,
        STT_BACKEND_CLOUD,
        "STT",
        Some(&config.base_url),
    )
    .map_err(|e| format!("{e:#}"))?;
    Ok(CloudTranscribeBackend::new_with_provider(config, provider))
}

/// Encode mono `f32` PCM at `sample_rate` Hz to a 16-bit PCM WAV byte
/// buffer -- the shape the OpenAI-compatible `/audio/transcriptions`
/// endpoint accepts. Samples are clamped to [-1.0, 1.0] before scaling to
/// avoid `i16` wrap on out-of-range input.
pub fn encode_wav_mono_16bit(pcm: &[f32], sample_rate: u32) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::<u8>::new());
    {
        let mut writer =
            hound::WavWriter::new(&mut cursor, spec).map_err(|e| format!("wav writer: {e}"))?;
        for &sample in pcm {
            let scaled = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
            writer
                .write_sample(scaled)
                .map_err(|e| format!("wav sample: {e}"))?;
        }
        writer
            .finalize()
            .map_err(|e| format!("wav finalize: {e}"))?;
    }
    Ok(cursor.into_inner())
}

/// Map a cloud `/audio/transcriptions` response onto the session's
/// [`TranscribeResult`], applying the whole-text hallucination blacklist.
///
/// Split out of [`CloudTranscribeBackend::transcribe`] so the mapping —
/// especially the `is_hallucination` assignment — is hermetically testable
/// without a live endpoint (the transcribe method's only untestable part
/// is the `cloud_transcribe` network call).
///
/// Runs the same whole-text hallucination gate Python applies in the
/// backend-agnostic `_transcribe_pcm` (`vp_dictate.py:379`), so the cloud
/// `stt_backend=openai` path filters `"tak"` / `"thank you"`-family credits
/// identically to local Whisper. The text is trimmed first: the endpoint
/// may return surrounding whitespace and the blacklist match rstrips only,
/// so a leading space would otherwise defeat it — mirroring the local
/// path's `normalize_whitespace` pre-step.
///
/// `stt_impl` is the provider label resolved from the configured base URL
/// (`cloud-openai` / `cloud-groq`) and is passed in rather than sniffed
/// here so this stays a pure mapping function. `stt_accel` is fixed at
/// `unknown`: the compute path is the provider's business and nothing in
/// the response reveals it -- claiming `cpu` or a GPU would be the exact
/// kind of guess the provenance fields exist to eliminate.
///
/// `requested_language` is the language the request ASKED the endpoint to
/// transcribe in; see the `language` resolution below for why the result has
/// to fall back to it.
#[cfg(test)]
fn map_cloud_result(
    result: CloudTranscriptionResult,
    latency_ms: u64,
    pcm_len: usize,
    sample_rate: u32,
    stt_impl: &str,
    requested_language: Option<&str>,
) -> TranscribeResult {
    map_cloud_result_with_max_cps(
        result,
        latency_ms,
        pcm_len,
        sample_rate,
        stt_impl,
        requested_language,
        max_chars_per_second_from_env(),
    )
}

fn map_cloud_result_with_max_cps(
    result: CloudTranscriptionResult,
    latency_ms: u64,
    pcm_len: usize,
    sample_rate: u32,
    stt_impl: &str,
    requested_language: Option<&str>,
    max_chars_per_second: f64,
) -> TranscribeResult {
    let duration_s = pcm_len as f64 / f64::from(sample_rate.max(1));
    // Impossible-speech-rate hallucination guard (Python's
    // `_exceeds_speech_rate` in `_transcribe_detail`): a transcript produced
    // far faster than real speech is blanked, so it surfaces as an `empty`
    // no-text event rather than injecting a hallucinated wall of text.
    let text = if speech_rate_exceeded(&result.text, duration_s, max_chars_per_second) {
        String::new()
    } else {
        result.text
    };
    let hallucinated = is_hallucination(text.trim());
    // Language reported for this utterance, in order: what the endpoint said,
    // else the language we ASKED it to transcribe in (the profile / config
    // hint `effective_language` resolved). #686 follow-up (Codex P1): the
    // standard `json` response format usually omits `language`, and an empty
    // value makes the post-processor keep its own configured `lang` — so a
    // profile that switched STT to `en` while the saved config says `da`
    // would still get a Danish cleanup prompt for an English transcript.
    // Mirrors the local backend, which reports `effective_language` directly.
    // Stays empty only when auto-detect ran AND the response said nothing,
    // which is the honest "unknown".
    let language = result
        .language
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .or_else(|| {
            requested_language
                .map(|l| l.trim().to_owned())
                .filter(|l| !l.is_empty())
        })
        .unwrap_or_default();
    TranscribeResult {
        text,
        language,
        latency_ms,
        duration_s,
        is_hallucination: hallucinated,
        gate: None,
        stt_impl: stt_impl.to_owned(),
        stt_accel: crate::whisper::Accel::Unknown.as_str().to_owned(),
        // Cloud STT does not surface a distinct raw_text or a
        // language probability -- both fields fall through to the
        // session's own fallback (raw_text <- source_text at event
        // build time; language_probability omitted from the payload
        // when 0.0). Codex P1 #606 metrics-schema follow-up.
        ..Default::default()
    }
}

/// Cloud STT backend. Holds a resolved [`CloudTranscribeConfig`] snapshot
/// (stamped at construction, like the session's other settings today), plus an
/// optional live-reloading STT prompt.
pub struct CloudTranscribeBackend {
    config: CloudTranscribeConfig,
    provider: String,
    nemotron_mode: bool,
    /// When set, the STT prompt is re-folded from `config.prompt` (treated as
    /// the BASE prompt) + the live dictionary terms on every `transcribe`, so
    /// dictionary term / budget edits re-bias STT without an app restart
    /// (Python's per-utterance `_dictionary_prompt_runtime`). `None` keeps the
    /// fixed `config.prompt`. `Mutex` because the reload cache mutates behind
    /// `transcribe(&self)`; boxed to keep the backend (and the
    /// `ProductionTranscribeBackend` enum) small when no reloading prompt is
    /// attached.
    prompt_reload: Option<Box<Mutex<crate::dictionary::ReloadingDictionary>>>,
    /// Per-utterance profile overrides (Codex P1 #607) -- see the sibling
    /// fields on [`super::WhisperLocalTranscribeBackend`] for the contract.
    /// A `model` override is reported through
    /// [`Self::profile_model_warned`] but not honoured (would require
    /// re-resolving `VOICEPI_STT_MODEL` + the api_key contract);
    /// documented as a follow-up.
    profile_prompt: Mutex<Option<String>>,
    profile_language: Mutex<Option<String>>,
    profile_model_warned: Mutex<Option<String>>,
    transcription_guards: Mutex<Option<TranscriptionGuards>>,
}

impl CloudTranscribeBackend {
    pub fn new(config: CloudTranscribeConfig) -> Self {
        Self::new_with_provider(config, "")
    }

    /// Construct a backend for NVIDIA Nemotron 3.5 ASR. The local NIM
    /// deployment uses `NIM_TAGS_SELECTOR=type=multi` to select the
    /// multilingual profile, while the request itself leaves
    /// `language_code` unset for automatic detection. Hosted Riva uses the
    /// same request contract.
    pub fn new_nemotron(config: CloudTranscribeConfig) -> Self {
        Self::new_with_provider(config, "nemotron")
    }

    /// Construct a backend with the selected cloud provider attached.
    ///
    /// An explicit provider is authoritative. Only legacy callers that pass
    /// an empty provider retain model-alias inference for Nemotron.
    pub fn new_with_provider(config: CloudTranscribeConfig, provider: &str) -> Self {
        let provider = provider.trim().to_owned();
        let nemotron_mode = crate::cloud_api::is_nemotron_provider(&provider)
            || (provider.is_empty() && is_nemotron_config(&config));
        Self {
            config,
            provider,
            nemotron_mode,
            prompt_reload: None,
            profile_prompt: Mutex::new(None),
            profile_language: Mutex::new(None),
            profile_model_warned: Mutex::new(None),
            transcription_guards: Mutex::new(None),
        }
    }

    /// Bind speech-gate and rate-limit settings to this in-process session.
    #[cfg(any(test, all(feature = "whisper-rs-local", feature = "rust-injection")))]
    pub(crate) fn with_transcription_guards(mut self, guards: TranscriptionGuards) -> Self {
        self.transcription_guards = Mutex::new(Some(guards));
        self
    }

    fn effective_transcription_guards(&self) -> TranscriptionGuards {
        self.transcription_guards
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .unwrap_or_else(TranscriptionGuards::from_env)
    }

    /// Attach a live-reloading STT prompt: `config.prompt` is treated as the
    /// BASE prompt and the dictionary terms are re-folded into it on each
    /// `transcribe`, under `precedence` (matching the session wiring:
    /// `EnvFirst` for `simulate-session`, `ConfigFirst` for the live worker).
    /// The caller must NOT pre-fold the terms into `config.prompt` -- pass the
    /// raw `VOICEPI_INITIAL_PROMPT` base so the terms can be re-folded live.
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
    #[cfg(any(test, all(feature = "whisper-rs-local", feature = "rust-injection")))]
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
    /// (highest first): profile override (Codex P1 #607), reload-prompt
    /// fold, fixed config prompt.
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
                .initial_prompt_with_terms(self.config.prompt.as_deref()),
            None => (self.config.prompt.clone(), Vec::new()),
        }
    }

    /// Language hint that will apply to the NEXT utterance: profile override
    /// wins over the config hint. Blank strings and the persisted `auto`
    /// sentinel are treated as "auto detect" and collapsed to `None`.
    /// Codex P1 #607.
    fn effective_language(&self) -> Option<String> {
        let profile = self
            .profile_language
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        profile
            .or_else(|| self.config.language.clone())
            .map(|s| s.trim().to_owned())
            .filter(|s| {
                !s.is_empty()
                    && !s.eq_ignore_ascii_case("auto")
                    // Older settings and callers used `multi` as a Nemotron
                    // deployment selector. It must never become Riva's
                    // request language_code (the service rejects it).
                    && !(self.nemotron_mode && s.eq_ignore_ascii_case("multi"))
            })
    }

    fn request_language(&self) -> Option<String> {
        // Nemotron's multilingual profile is selected at deployment time;
        // automatic language detection is represented by an omitted request
        // language, not by the `multi` deployment tag.
        self.effective_language()
    }

    /// Read-only view of the resolved config (tests / diagnostics).
    pub fn config(&self) -> &CloudTranscribeConfig {
        &self.config
    }

    pub fn stt_impl(&self) -> &'static str {
        if self.nemotron_mode {
            crate::dictate::provenance::STT_IMPL_CLOUD_NEMOTRON
        } else {
            crate::dictate::provenance::cloud_stt_impl_for_base_url(&self.config.base_url)
        }
    }
}

impl TranscribeBackend for CloudTranscribeBackend {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        // Full pre-model pipeline of Python's `vp_transcribe._transcribe_detail`
        // (`vp_transcribe.py:1255-1267`): trim the trailing dead-air tail ONCE,
        // gate the trimmed buffer (reject too-quiet / no-contrast audio BEFORE
        // the network call), and boost the quiet body toward the target level.
        // `duration_s` comes from the trimmed length; the gate reason flows onto
        // `TranscribeResult.gate`, which the session maps to a
        // `too_quiet`/`no_speech` no-text event. Sending the untrimmed tail to
        // the endpoint would give it empty audio to hallucinate a caption over
        // and inflate the chars-per-second denominator of the speech-rate guard.
        let guards = self.effective_transcription_guards();
        let audio =
            match crate::audio_dsp::prepare_for_transcription(pcm, sample_rate, &guards.thresholds)
            {
                crate::audio_dsp::PreparedAudio::Reject { reason, duration_s } => {
                    return Ok(TranscribeResult {
                        text: String::new(),
                        gate: Some(reason),
                        duration_s,
                        ..Default::default()
                    });
                }
                crate::audio_dsp::PreparedAudio::Decode { audio, .. } => audio,
            };
        // Encode + POST the prepared (trimmed + boosted) audio. `map_cloud_result`
        // derives `duration_s` from its length -- the boost is gain-only, so the
        // length still reflects the trimmed clip the gate measured.
        let pcm = audio.as_slice();
        let wav = encode_wav_mono_16bit(pcm, sample_rate)
            .map_err(|e| TranscribeError::Backend(format!("wav encode failed: {e}")))?;
        // Re-fold the dictionary terms into the prompt per utterance when a
        // reloading prompt is attached (else the fixed config prompt). The
        // profile override (Codex P1 #607) wins over both in `effective_*`.
        let (prompt, dictionary_terms) = self.effective_prompt();
        let effective_language = self.effective_language();
        let request_language = self.request_language();
        let started = Instant::now();
        let request = CloudTranscriptionRequest::new(
            &self.config.base_url,
            &self.config.api_key,
            &self.config.model,
            &wav,
            request_language.as_deref(),
            prompt.as_deref(),
            self.config.timeout_ms,
        );
        let result = if self.nemotron_mode {
            let provider = if self.provider.is_empty() {
                "nemotron"
            } else {
                self.provider.as_str()
            };
            cloud_transcribe_for_provider(provider, request)
        } else {
            cloud_transcribe(
                &self.config.base_url,
                &self.config.api_key,
                &self.config.model,
                &wav,
                request_language.as_deref(),
                prompt.as_deref(),
                self.config.timeout_ms,
            )
        }
        .map_err(|e| TranscribeError::Backend(format!("cloud transcription failed: {e:#}")))?;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut result = map_cloud_result_with_max_cps(
            result,
            latency_ms,
            pcm.len(),
            sample_rate,
            // Sniffed from the LIVE base URL, not from `stt_backend` --
            // which spells `openai` for Groq too, so the setting alone
            // cannot tell the two providers apart.
            self.stt_impl(),
            effective_language.as_deref(),
            guards.max_chars_per_second,
        );
        result.dictionary_terms =
            (!dictionary_terms.is_empty()).then(|| dictionary_terms.into_boxed_slice());
        Ok(result)
    }

    fn apply_profile_overrides(&self, settings: &std::collections::BTreeMap<String, String>) {
        if let Some(guards) = self
            .transcription_guards
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
        {
            guards.apply_settings(settings);
        }
        if let Some(reload) = self.prompt_reload.as_ref() {
            crate::dictionary::DictionaryProvider::apply_settings(
                &mut *reload
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
                settings,
            );
        }
        // Mirror `WhisperLocalTranscribeBackend`'s handling so a profile
        // that flips STT provider mid-session sees the same overrides.
        let prompt_override = settings
            .get("initial_prompt")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        *self
            .profile_prompt
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = prompt_override;
        let language_override = settings
            .get("language")
            .or_else(|| settings.get("lang"))
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        *self
            .profile_language
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = language_override;
        // `model`: DEFERRED. The api_key contract is provider-scoped
        // (openai vs groq); a profile-driven model swap that crosses
        // providers also needs a re-resolved key. Warn once per new
        // value, matching `WhisperLocalTranscribeBackend`.
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
                     (cloud STT backend model is stamped at construction; \
                     restart the app for a `model` profile override to take effect)",
                    model
                );
                *warned = Some(model.to_owned());
            }
        } else {
            *self
                .profile_model_warned
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = None;
        }
    }
}

#[cfg(test)]
#[path = "cloud_transcribe_tests.rs"]
mod tests;
