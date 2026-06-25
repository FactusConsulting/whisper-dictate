//! JSON-envelope dispatcher for the hidden `transcribe-wav` sub-command.
//!
//! This is the runtime-wiring layer for Phase 1.2 of the Python-removal
//! roadmap (#348). The Python `vp_transcribe.py` worker shells out to
//! `whisper-dictate transcribe-wav` (only when `VOICEPI_TRANSCRIBE_BACKEND=rust`
//! is set — see the constraint in the PR description) and reads the resulting
//! transcript back from stdout. By keeping the protocol JSON-over-stdio we
//! match the same shape every other Rust↔Python helper uses (`health`,
//! `redact-text`, `apply-profile`, `privacy`, `dictionary-runtime`), so the
//! existing Python subprocess wrapper code carries over almost verbatim.
//!
//! Request envelope (single JSON object on stdin):
//!
//! ```json
//! {
//!   "action": "transcribe_wav",
//!   "wav_path": "/tmp/voicepi-utterance-1234.wav",
//!   "language": "en",                  // optional; "auto" / "" / null → auto-detect
//!   "initial_prompt": "Codex Aurelia"  // optional; "" / null → no prompt
//! }
//! ```
//!
//! Response envelope (single JSON object on stdout):
//!
//! ```json
//! { "text": "hello world" }
//! ```
//!
//! On error the process exits non-zero and writes the message to stderr — the
//! same convention the other helpers use, so the Python fallback path (drop
//! the rust backend and surface the error to the user) doesn't need a special
//! case for this command.
//!
//! The model file is located from `VOICEPI_WHISPER_MODEL_PATH`. Resolving it
//! lazily (per-process) rather than at module load means tests can override
//! the env var without re-importing.

use std::env;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::whisper::LocalWhisper;

/// Env var the Python wiring sets to point at a downloaded GGML model file.
pub const MODEL_PATH_ENV: &str = "VOICEPI_WHISPER_MODEL_PATH";

/// JSON request envelope. Matches the documented shape exactly; unknown
/// fields are rejected by serde so a future schema bump can't silently
/// produce wrong output.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranscribeRequest {
    /// Transcribe a 16 kHz mono WAV file from disk and return its text.
    TranscribeWav {
        wav_path: String,
        #[serde(default)]
        language: Option<String>,
        #[serde(default)]
        initial_prompt: Option<String>,
    },
}

/// JSON response envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscribeResponse {
    pub text: String,
}

/// Entry point used by `main.rs`. Reads one JSON request from stdin, runs
/// inference, and prints one JSON response to stdout.
pub fn handle_transcribe_wav() -> Result<()> {
    let request = read_request_from_reader(&mut io::stdin().lock())?;
    let model_path = resolve_model_path_from_env()?;
    let response = dispatch(&model_path, request)?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

/// Resolve the model file path from `VOICEPI_WHISPER_MODEL_PATH`.
///
/// We do not silently fall back to a default cache directory: Phase 1.2 is
/// only entered when the Python side has explicitly opted in via
/// `VOICEPI_TRANSCRIBE_BACKEND=rust`, and that side already knows which model
/// file it intends to use. Surfacing a clear "missing env var" error here is
/// far less confusing than picking some arbitrary `~/.cache` path that may
/// not exist or hold the model the user expects. A later sub-issue (Wave 7
/// model download UI) will own the default location story.
fn resolve_model_path_from_env() -> Result<PathBuf> {
    let raw = env::var(MODEL_PATH_ENV).map_err(|_| {
        anyhow!(
            "{MODEL_PATH_ENV} is not set; the Rust transcription backend needs \
             a GGML whisper.cpp model path (e.g. ggml-small.en.bin)"
        )
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "{MODEL_PATH_ENV} is set but empty; point it at a GGML \
             whisper.cpp model file"
        ));
    }
    Ok(PathBuf::from(trimmed))
}

/// Pure helper used by `handle_transcribe_wav`. Split out so the CLI plumbing
/// (stdin read, stdout print, env-var resolve) stays separable from the
/// actual model load + inference call — which is the only piece that touches
/// whisper.cpp and is therefore awkward to unit-test without a real model.
fn dispatch(model_path: &Path, request: TranscribeRequest) -> Result<TranscribeResponse> {
    match request {
        TranscribeRequest::TranscribeWav {
            wav_path,
            language,
            initial_prompt,
        } => {
            let whisper = LocalWhisper::new(model_path).with_context(|| {
                format!("failed to load whisper model from {}", model_path.display())
            })?;
            // Normalise the language and prompt at the dispatch boundary so
            // the library layer below only sees the states it cares about.
            // The mirror of this normalisation in `LocalWhisper::transcribe_*`
            // is belt-and-braces — keeping both means a direct library caller
            // (the example binary, future test code) still gets the right
            // behaviour on an empty-string prompt.
            //
            // The two fields normalise differently: `language` has an `auto`
            // sentinel meaning "let the model detect" (mapped to None), but
            // `initial_prompt` is free-form user text — collapsing a literal
            // "auto" prompt to None would silently drop a valid user prompt
            // that happens to equal that word (it would still be applied on
            // the faster-whisper path).
            let lang_for_call = normalise_language(language.as_deref());
            let prompt_for_call = normalise_prompt(initial_prompt.as_deref());
            let text =
                whisper.transcribe_wav(Path::new(&wav_path), lang_for_call, prompt_for_call)?;
            Ok(TranscribeResponse { text })
        }
    }
}

/// Treat `None`, `Some("")`, and `Some("auto")` as "no language pinned" — the
/// `auto` sentinel is meaningful here and mirrors the Python config UI.
fn normalise_language(value: Option<&str>) -> Option<&str> {
    match value {
        None => None,
        Some("") | Some("auto") => None,
        Some(other) => Some(other),
    }
}

/// Treat only `None` and `Some("")` as "no prompt". A literal `"auto"` is a
/// valid prompt token (the user or a dictionary entry might legitimately ask
/// the model to bias toward that word) and must reach the inference call
/// unchanged, matching the faster-whisper path's behaviour.
fn normalise_prompt(value: Option<&str>) -> Option<&str> {
    match value {
        None => None,
        Some("") => None,
        Some(other) => Some(other),
    }
}

/// Parse a JSON request from an arbitrary reader. Exposed for unit tests so
/// they don't have to splice stdin.
fn read_request_from_reader<R: Read>(reader: &mut R) -> Result<TranscribeRequest> {
    let mut raw = String::new();
    reader
        .read_to_string(&mut raw)
        .context("failed to read transcribe-wav request from stdin")?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse transcribe-wav JSON request: {raw}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock::ENV_LOCK;

    #[test]
    fn parses_minimal_request() {
        let mut input = br#"{"action":"transcribe_wav","wav_path":"/tmp/a.wav"}"#.as_slice();
        let req = read_request_from_reader(&mut input).unwrap();
        assert_eq!(
            req,
            TranscribeRequest::TranscribeWav {
                wav_path: "/tmp/a.wav".to_owned(),
                language: None,
                initial_prompt: None,
            }
        );
    }

    #[test]
    fn parses_request_with_language_and_prompt() {
        let json = br#"{
            "action": "transcribe_wav",
            "wav_path": "C:/Users/foo/u.wav",
            "language": "da",
            "initial_prompt": "Codex Aurelia"
        }"#;
        let mut input = json.as_slice();
        let req = read_request_from_reader(&mut input).unwrap();
        assert_eq!(
            req,
            TranscribeRequest::TranscribeWav {
                wav_path: "C:/Users/foo/u.wav".to_owned(),
                language: Some("da".to_owned()),
                initial_prompt: Some("Codex Aurelia".to_owned()),
            }
        );
    }

    #[test]
    fn rejects_unknown_action() {
        let mut input = br#"{"action":"do_the_thing","wav_path":"x"}"#.as_slice();
        let err = read_request_from_reader(&mut input).unwrap_err();
        assert!(
            err.to_string().contains("transcribe-wav JSON request"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_unknown_field_to_prevent_silent_schema_drift() {
        // deny_unknown_fields guards against the case where a future Python
        // worker sends a new key (e.g. `temperature`) that this build doesn't
        // honour: we'd rather fail loudly so the user updates the Rust binary
        // than silently ignore the request and produce wrong output.
        let json = br#"{
            "action": "transcribe_wav",
            "wav_path": "/tmp/a.wav",
            "temperature": 0.0
        }"#;
        let mut input = json.as_slice();
        let err = read_request_from_reader(&mut input).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("temperature")
                || err.to_string().contains("unknown field"),
            "expected unknown-field error, got: {err}"
        );
    }

    #[test]
    fn rejects_empty_input() {
        let mut input = b"".as_slice();
        let err = read_request_from_reader(&mut input).unwrap_err();
        assert!(
            err.to_string().contains("transcribe-wav JSON request"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn normalise_language_collapses_empty_and_auto() {
        assert_eq!(normalise_language(None), None);
        assert_eq!(normalise_language(Some("")), None);
        assert_eq!(normalise_language(Some("auto")), None);
        assert_eq!(normalise_language(Some("en")), Some("en"));
        assert_eq!(normalise_language(Some("da")), Some("da"));
    }

    #[test]
    fn normalise_prompt_preserves_literal_auto() {
        // None and empty collapse, but a literal "auto" is a valid prompt
        // (user/dictionary may inject the word) — it must reach the model
        // unchanged so behaviour matches the faster-whisper path.
        assert_eq!(normalise_prompt(None), None);
        assert_eq!(normalise_prompt(Some("")), None);
        assert_eq!(normalise_prompt(Some("auto")), Some("auto"));
        assert_eq!(normalise_prompt(Some("Codex")), Some("Codex"));
    }

    #[test]
    fn resolve_model_path_errors_when_env_missing() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = env::var(MODEL_PATH_ENV).ok();
        env::remove_var(MODEL_PATH_ENV);

        let err = resolve_model_path_from_env().unwrap_err();
        assert!(
            err.to_string().contains(MODEL_PATH_ENV) && err.to_string().contains("not set"),
            "unexpected error: {err}"
        );

        if let Some(v) = saved {
            env::set_var(MODEL_PATH_ENV, v);
        }
    }

    #[test]
    fn resolve_model_path_errors_when_env_blank() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = env::var(MODEL_PATH_ENV).ok();
        env::set_var(MODEL_PATH_ENV, "   ");

        let err = resolve_model_path_from_env().unwrap_err();
        assert!(err.to_string().contains("empty"), "unexpected error: {err}");

        match saved {
            Some(v) => env::set_var(MODEL_PATH_ENV, v),
            None => env::remove_var(MODEL_PATH_ENV),
        }
    }

    #[test]
    fn resolve_model_path_returns_value_when_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = env::var(MODEL_PATH_ENV).ok();
        env::set_var(MODEL_PATH_ENV, "/tmp/ggml-tiny.en.bin");

        let p = resolve_model_path_from_env().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/ggml-tiny.en.bin"));

        match saved {
            Some(v) => env::set_var(MODEL_PATH_ENV, v),
            None => env::remove_var(MODEL_PATH_ENV),
        }
    }

    /// End-to-end: with a real model + WAV (env-gated, same as the
    /// `transcribes_hello_world_when_model_available` test in mod.rs),
    /// run the CLI dispatch path and assert the JSON envelope is well-formed
    /// and contains the expected substring. Skipped unless the developer
    /// opts in via the two env vars below.
    #[test]
    fn dispatches_real_transcription_when_model_available() {
        let (Ok(model), Ok(wav)) = (
            env::var("WHISPER_TEST_MODEL_PATH"),
            env::var("WHISPER_TEST_WAV_PATH"),
        ) else {
            eprintln!(
                "skipping: set WHISPER_TEST_MODEL_PATH (GGML whisper model) and \
                 WHISPER_TEST_WAV_PATH (16 kHz mono 'hello world' WAV) to run"
            );
            return;
        };

        let request = TranscribeRequest::TranscribeWav {
            wav_path: wav,
            language: None,
            initial_prompt: None,
        };
        let response = dispatch(Path::new(&model), request).expect("dispatch ok");
        assert!(
            response.text.to_lowercase().contains("hello"),
            "transcript missing 'hello': {:?}",
            response.text
        );
    }
}
