//! Post-STT formatting / LLM cleanup (Rust port of `vp_postprocess.py`).
//!
//! Wave 4-B of the Python-removal roadmap (#348). Owns the same flow the
//! Python module did:
//!
//! 1. settings validation + local-only check (delegated to [`crate::privacy`]);
//! 2. optional cloud-safe redaction (delegated to [`crate::redaction`]);
//! 3. prompt construction (`prompt::build_prompt`);
//! 4. provider call — local Ollama (`/api/generate`) or OpenAI-compatible
//!    chat completion ([`crate::cloud_api::openai_chat_completion`]);
//! 5. final-text extraction (`prompt::extract_final_text`) and redaction
//!    restore;
//! 6. byte-length cap + idempotent fall-through to the original text.
//!
//! The pure helpers (`normalize_mode`, `build_prompt`, `extract_final_text`,
//! `effective_timeout_ms`, `normalized_model`, `normalized_base_url`) are
//! exposed for unit tests so each transformation is covered without spinning
//! up an HTTP server.
//!
//! A `postprocess` subcommand is wired in `cli.rs` / `main.rs`; Python
//! `vp_postprocess.py` shells out when `VOICEPI_POSTPROCESS_BACKEND=rust`
//! (and falls back to the in-process path on any error so default install
//! behaviour stays byte-identical).
//!
//! Submodules:
//! * [`prompt`] — pure-string helpers (mode normalisation, prompt
//!   construction, before/becomes/after extraction).
//! * [`settings`] — config types, defaults, normalisation, validation.
//! * [`run`] — the pipeline itself + HTTP backends.

mod prompt;
mod run;
mod settings;

pub use prompt::{
    build_prompt, extract_final_text, language_instruction, normalize_mode, sanitize_lang,
};
pub use run::{
    effective_timeout_ms, postprocess_text, PostprocessResult, RedactionSummary, CEILING_MS,
    PER_CHAR_MS,
};
pub use settings::{
    default_base_url, looks_like_http_url, normalized_base_url, normalized_model,
    settings_from_env, settings_from_env_with, PostprocessSettings, DEFAULT_OLLAMA_BASE_URL,
    DEFAULT_OLLAMA_POST_MODEL, POST_PROCESSOR_ENV, VALID_MODES, VALID_PROCESSORS,
};

use std::io::{self, Read};
use std::sync::Mutex;

use anyhow::Result;
use serde::Deserialize;

use crate::dictate::{PostProcessBackend, PostProcessOutcome, PostRedaction};

/// Adapter that drives the full [`postprocess_text`] pipeline as a session
/// [`crate::dictate::PostProcessBackend`], so the in-process Rust engine can
/// run the same LLM cleanup pass the Python worker did -- without a Python
/// child building the settings envelope.
///
/// Holds a snapshot of [`PostprocessSettings`] stamped at construction
/// (like the session's other live settings today; a per-utterance re-read
/// is deferred to the same follow-up that refreshes the audio-route env).
/// `post_process` returns [`PostprocessResult::text`], which
/// [`postprocess_text`] guarantees falls back to the input text on any
/// provider / transport error or empty rewrite -- so attaching this backend
/// can never drop the user's dictation, only improve it.
pub struct SessionPostProcess {
    /// Live settings the pass consults on every utterance. Wrapped in
    /// [`Mutex`] so the profile-matcher (Codex P1 #607) can overwrite
    /// selected keys mid-session via
    /// [`PostProcessBackend::apply_profile_overrides`] without rebuilding
    /// the backend. The BASE snapshot is preserved separately so a
    /// non-matching profile can RESET the overrides on the next
    /// utterance.
    settings: Mutex<PostprocessSettings>,
    /// Immutable snapshot of the settings stamped at construction, kept
    /// so [`Self::apply_profile_overrides`] can reset to the base when
    /// the profile does not carry a given key -- mirroring the
    /// per-utterance `base_config` reset in `DictateSession::apply_active_profile`.
    base_settings: PostprocessSettings,
}

impl SessionPostProcess {
    /// Wrap an explicit settings snapshot (used by tests and by
    /// [`Self::from_settings`]).
    pub fn new(settings: PostprocessSettings) -> Self {
        Self {
            base_settings: settings.clone(),
            settings: Mutex::new(settings),
        }
    }

    /// Build from a settings snapshot. Codex P1 #607: this used to return
    /// `None` when the processor was `none` / the mode was `raw`, which
    /// meant a profile that flipped `post_processor=ollama` mid-session
    /// had NO backend attached and its override was silently dropped.
    /// The session now gates the pass on [`PostProcessBackend::is_active`]
    /// so the backend is always attached and the profile can enable it.
    pub fn from_settings(settings: PostprocessSettings) -> Self {
        Self::new(settings)
    }

    /// Build from the process environment (the `VOICEPI_POST_*` vars the UI
    /// exports into the worker env). Codex P1 #607: always returns `Self`
    /// so the session has a target for [`Self::apply_profile_overrides`].
    /// A default (unset) env still runs [`Self::is_active`] returning
    /// `false`, so a stock config pays zero per-utterance cost.
    pub fn from_env() -> Self {
        Self::from_settings(settings_from_env())
    }

    /// Test-only: snapshot the current (post-override) settings.
    #[cfg(test)]
    pub(crate) fn current_settings(&self) -> PostprocessSettings {
        self.settings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The settings ONE utterance runs with: the live snapshot with the
    /// effective per-utterance language stamped over the configured one.
    ///
    /// #686 follow-up (Codex P1): `settings.lang` is stamped once from the
    /// process env (plus a profile `lang` key), but the language STT actually
    /// used can differ for a single utterance — a `--lang` / profile override,
    /// or the language the model detected when running on auto-detect. The
    /// cleanup prompt names this language, so building it from the stale
    /// config value would let the prompt assert a language the transcript is
    /// not in — recreating the translation bug #685 fixed, from the other
    /// side. An empty `lang` means the backend reported nothing, and then the
    /// configured hint is the best we have (mirrors Python's
    /// `result.language or self.lang`).
    fn utterance_settings(&self, lang: &str) -> PostprocessSettings {
        let mut settings = self
            .settings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let effective = lang.trim();
        if !effective.is_empty() {
            settings.lang = effective.to_owned();
        }
        settings
    }
}

impl PostProcessBackend for SessionPostProcess {
    fn post_process(&self, text: &str, lang: &str) -> PostProcessOutcome {
        // `lang` is the language STT ACTUALLY used for this utterance; see
        // [`Self::utterance_settings`] for why it wins over the configured
        // one (#686 follow-up).
        let settings_snapshot = self.utterance_settings(lang);
        let result = postprocess_text(text, &settings_snapshot);
        let redactions = result
            .redactions
            .into_iter()
            .map(|r| PostRedaction {
                placeholder: r.placeholder,
                kind: r.kind,
                chars: r.chars,
            })
            .collect();
        PostProcessOutcome {
            text: result.text,
            processor: result.provider,
            mode: result.mode,
            model: result.model,
            latency_ms: result.latency_ms,
            changed: result.changed,
            fallback: result.fallback,
            error: result.error,
            redacted: result.redacted,
            redactions,
        }
    }

    fn is_active(&self) -> bool {
        // Python parity: post-processing runs when a processor is
        // configured AND the mode is not `raw`.
        let settings = self.settings.lock().unwrap_or_else(|p| p.into_inner());
        settings.processor != "none" && normalize_mode(&settings.mode) != "raw"
    }

    fn apply_profile_overrides(&self, profile: &std::collections::BTreeMap<String, String>) {
        // Reset to the base snapshot FIRST so overrides from a PREVIOUS
        // utterance's profile do not leak into this one when the current
        // profile carries a different (or empty) set of `post_*` keys.
        // Mirrors the `SessionConfig` reset in `apply_active_profile`.
        let mut settings = self.settings.lock().unwrap_or_else(|p| p.into_inner());
        *settings = self.base_settings.clone();
        // Each key: profile trims + normalises + overrides the field.
        // Blank / whitespace-only values are treated as "unset" (fall
        // through to the base) matching the `settings_from_env_with`
        // treatment. Unknown numeric strings fall through to the base
        // (permissive, matches Python's config-layer coercion).
        if let Some(processor) = profile
            .get("post_processor")
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty() && VALID_PROCESSORS.contains(&v.as_str()))
        {
            settings.processor = processor;
        }
        if let Some(mode) = profile
            .get("post_mode")
            .map(|v| normalize_mode(v.trim()))
            .filter(|v| VALID_MODES.contains(&v.as_str()))
        {
            settings.mode = mode;
        }
        if let Some(model) = profile
            .get("post_model")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
        {
            settings.model = normalized_model(&settings.processor, &model);
        } else {
            // Re-normalise the base model against a (possibly overridden)
            // processor so a profile that flips ollama -> groq keeps the
            // groq default when no explicit `post_model` is set.
            settings.model = normalized_model(&settings.processor, &self.base_settings.model);
        }
        if let Some(base_url) = profile
            .get("post_base_url")
            .map(|v| v.trim().trim_end_matches('/').to_owned())
            .filter(|v| !v.is_empty())
        {
            settings.base_url = normalized_base_url(&settings.processor, &base_url);
        } else {
            settings.base_url =
                normalized_base_url(&settings.processor, &self.base_settings.base_url);
        }
        if let Some(timeout) = profile
            .get("post_timeout_ms")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .map(|v| (v.trunc().max(0.0) as u64).max(100))
        {
            settings.timeout_ms = timeout;
        }
        if let Some(max_in) = profile
            .get("post_max_input_chars")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .map(|v| (v.trunc().max(0.0) as u64).max(100) as usize)
        {
            settings.max_input_chars = max_in;
        }
        if let Some(max_out) = profile
            .get("post_max_output_chars")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .map(|v| (v.trunc().max(0.0) as u64).max(100) as usize)
        {
            settings.max_output_chars = max_out;
        }
        if let Some(redact) = profile.get("post_redact") {
            settings.redact = crate::dictate::is_truthy(Some(redact.trim()));
        }
        if let Some(terms) = profile
            .get("post_redact_terms")
            .map(|v| v.trim().to_owned())
        {
            settings.redact_terms = terms;
        }
        // #685: a profile that switches the spoken language (e.g. an English
        // work app while the base config is Danish) must switch the language
        // the cleanup prompt asks the model to reply in, or the pass would be
        // told to preserve the wrong language. Blank = auto-detect, which the
        // prompt still binds to "the same language as the input".
        if let Some(lang) = profile.get("lang").map(|v| v.trim().to_owned()) {
            settings.lang = lang;
        }
    }
}

/// JSON envelope for the hidden `postprocess` subcommand. Mirrors the
/// `health` envelope shape: a single top-level `action` discriminator that
/// selects which helper runs against the rest of the payload.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum PostprocessRequest {
    /// Full pipeline (validate → redact → LLM → restore → cap). Returns a
    /// [`PostprocessResult`].
    Process {
        text: String,
        settings: PostprocessSettings,
    },
    BuildPrompt {
        text: String,
        mode: String,
        /// Configured spoken-language hint. Optional so an older caller's
        /// payload still deserialises (#685).
        #[serde(default)]
        lang: String,
    },
    ExtractFinalText {
        output: String,
        source_text: String,
    },
    EffectiveTimeout {
        base_ms: u64,
        text_chars: i64,
    },
    NormalizeMode {
        mode: String,
    },
}

pub fn handle_postprocess() -> Result<()> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let request: PostprocessRequest = serde_json::from_str(&raw)?;
    match request {
        PostprocessRequest::Process { text, settings } => {
            let result = postprocess_text(&text, &settings);
            println!("{}", serde_json::to_string(&result)?);
        }
        PostprocessRequest::BuildPrompt { text, mode, lang } => {
            let response = serde_json::json!({"prompt": build_prompt(&text, &mode, &lang)});
            println!("{}", serde_json::to_string(&response)?);
        }
        PostprocessRequest::ExtractFinalText {
            output,
            source_text,
        } => {
            let response = serde_json::json!({"text": extract_final_text(&output, &source_text)});
            println!("{}", serde_json::to_string(&response)?);
        }
        PostprocessRequest::EffectiveTimeout {
            base_ms,
            text_chars,
        } => {
            let response =
                serde_json::json!({"timeout_ms": effective_timeout_ms(base_ms, text_chars)});
            println!("{}", serde_json::to_string(&response)?);
        }
        PostprocessRequest::NormalizeMode { mode } => {
            let response = serde_json::json!({"mode": normalize_mode(&mode)});
            println!("{}", serde_json::to_string(&response)?);
        }
    }
    Ok(())
}

// Companion test module. The inline `#[cfg(test)] mod session_backend_tests`
// block lived here; it moved to `mod_tests.rs` so the regression-test
// discipline scanner (which resolves `mod.rs` -> `mod_tests.rs`) finds a
// matching companion file, and so this module stays small.
#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
