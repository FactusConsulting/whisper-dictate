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
    PostprocessSettings, DEFAULT_OLLAMA_BASE_URL, DEFAULT_OLLAMA_POST_MODEL, VALID_MODES,
    VALID_PROCESSORS,
};

use std::io::{self, Read};

use anyhow::Result;
use serde::Deserialize;

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
