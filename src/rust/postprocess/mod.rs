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

pub use prompt::{build_prompt, extract_final_text, normalize_mode};
pub use run::{
    effective_timeout_ms, postprocess_text, PostprocessResult, RedactionSummary, CEILING_MS,
    PER_CHAR_MS,
};
pub use settings::{
    default_base_url, looks_like_http_url, normalized_base_url, normalized_model,
    settings_from_env, settings_from_env_with, PostprocessSettings, DEFAULT_OLLAMA_BASE_URL,
    DEFAULT_OLLAMA_POST_MODEL, VALID_MODES, VALID_PROCESSORS,
};

use std::io::{self, Read};

use anyhow::Result;
use serde::Deserialize;

use crate::dictate::PostProcessBackend;

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
    settings: PostprocessSettings,
}

impl SessionPostProcess {
    /// Wrap an explicit settings snapshot (used by tests and by
    /// [`Self::from_settings`]).
    pub fn new(settings: PostprocessSettings) -> Self {
        Self { settings }
    }

    /// Build from a settings snapshot, returning `None` when no
    /// post-processor is configured (`processor == "none"`) so the caller
    /// skips attaching a backend entirely -- zero per-utterance overhead
    /// and no `post-processing` status for the default config.
    pub fn from_settings(settings: PostprocessSettings) -> Option<Self> {
        if settings.processor == "none" {
            None
        } else {
            Some(Self::new(settings))
        }
    }

    /// Build from the process environment (the `VOICEPI_POST_*` vars the UI
    /// exports into the worker env). `None` when the operator has not
    /// enabled a post-processor.
    pub fn from_env() -> Option<Self> {
        Self::from_settings(settings_from_env())
    }
}

impl PostProcessBackend for SessionPostProcess {
    fn post_process(&self, text: &str) -> String {
        postprocess_text(text, &self.settings).text
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
        PostprocessRequest::BuildPrompt { text, mode } => {
            let response = serde_json::json!({"prompt": build_prompt(&text, &mode)});
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

#[cfg(test)]
mod session_backend_tests {
    use super::*;

    fn settings(processor: &str) -> PostprocessSettings {
        let mut s = settings_from_env_with(|_| None);
        s.processor = processor.to_owned();
        s
    }

    #[test]
    fn from_settings_returns_none_for_disabled_processor() {
        assert!(SessionPostProcess::from_settings(settings("none")).is_none());
        assert!(SessionPostProcess::from_settings(settings("ollama")).is_some());
    }

    #[test]
    fn post_process_is_passthrough_when_processor_none() {
        // A `none` processor never touches the network: `post_process`
        // returns the input verbatim. (The backend would normally be
        // skipped via `from_settings` -> None, but constructing it directly
        // pins the passthrough contract.)
        let backend = SessionPostProcess::new(settings("none"));
        assert_eq!(backend.post_process("keep me exactly"), "keep me exactly");
    }

    #[test]
    fn post_process_falls_back_to_input_on_unreachable_provider() {
        // Ollama pointed at a closed port fails fast and
        // `postprocess_text` falls back to the original text -- the seam
        // must never drop the user's dictation. Mirrors run.rs's
        // `ollama_failure_falls_back_to_original_text`.
        let mut s = settings("ollama");
        s.mode = "clean".to_owned();
        s.base_url = "http://127.0.0.1:1".to_owned();
        s.timeout_ms = 100;
        let backend = SessionPostProcess::new(s);
        assert_eq!(backend.post_process("dictated text"), "dictated text");
    }
}
