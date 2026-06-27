//! JSON envelope + line-server protocol shared by `transcribe-wav` and
//! `transcribe-server`.
//!
//! Carries the always-compiled pieces of the Rust↔Python transcribe IPC so
//! they can be unit-tested on every CI run without pulling in
//! whisper.cpp. The whisper-rs-local-gated [`super::dispatch`] module
//! wires these helpers up to [`super::LocalWhisper`] (single-shot) and
//! [`super::IdleUnloadingModel`] (long-running server).
//!
//! ## Wire formats
//!
//! Both modes share the **request envelope** — a single JSON object,
//! either alone on stdin (`transcribe-wav`) or one per `\n`-terminated
//! line (`transcribe-server`):
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
//! Single-shot `transcribe-wav` emits exactly one **success response** to
//! stdout and exits with status 0; failures exit non-zero and write to
//! stderr. The server emits a [`ServerReady`] line first, then one
//! response per request — **either** a [`TranscribeResponse`] or a JSON
//! error envelope `{"error": "<message>"}` so a single bad request does
//! not tear down the server.
//!
//! Wave 8-A of #348 (in-process whisper-rs worker).

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// JSON request envelope. Matches the documented shape exactly; unknown
/// fields are rejected by serde so a future schema bump can't silently
/// produce wrong output on an old binary.
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

/// JSON response envelope for a single transcription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscribeResponse {
    pub text: String,
}

/// First line the server emits on stdout — confirms the binary supports the
/// long-running mode and reports the resolved config so the Python wrapper
/// can log/verify it.
///
/// `idle_unload_s = 0` means "never unload" (the historical contract from
/// [`super::IDLE_UNLOAD_ENV`]). `model_path` echoes the path the server
/// will lazy-load on the first transcribe call.
#[derive(Debug, Clone, Serialize)]
pub struct ServerReady {
    pub ready: bool,
    pub model_path: String,
    pub idle_unload_s: u64,
}

/// Treat `None`, `Some("")`, and `Some("auto")` as "no language pinned" — the
/// `auto` sentinel is meaningful here and mirrors the Python config UI.
pub fn normalise_language(value: Option<&str>) -> Option<&str> {
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
pub fn normalise_prompt(value: Option<&str>) -> Option<&str> {
    match value {
        None => None,
        Some("") => None,
        Some(other) => Some(other),
    }
}

/// Parse a JSON request from an arbitrary reader. Exposed for unit tests so
/// they don't have to splice stdin, and reused by the single-shot
/// `transcribe-wav` path which slurps the full reader.
pub fn read_request_from_reader<R: std::io::Read>(reader: &mut R) -> Result<TranscribeRequest> {
    let mut raw = String::new();
    reader
        .read_to_string(&mut raw)
        .context("failed to read transcribe-wav request from stdin")?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse transcribe-wav JSON request: {raw}"))
}

/// Long-running line-server loop driving the `transcribe-server` subcommand.
///
/// Reads one JSON request per line from `reader`, runs `transcribe` against
/// the request (a closure so the loop is generic over the model type), and
/// writes one JSON response per line to `writer`, flushing after each so
/// the Python wrapper sees output immediately.
///
/// Per-request errors are encoded as `{"error": "<message>"}` JSON envelopes
/// and the server CONTINUES — a single bad request must not tear down the
/// long-running worker (which would defeat the whole point of caching the
/// loaded model between calls). A failure to READ from stdin or WRITE to
/// stdout is fatal: those represent pipe shutdown, which means the
/// supervisor has gone away.
///
/// Returns `Ok(())` on clean EOF.
pub fn serve_loop<R, W, F>(reader: R, mut writer: W, transcribe: F) -> Result<()>
where
    R: BufRead,
    W: Write,
    F: Fn(&str, Option<&str>, Option<&str>) -> Result<String>,
{
    for line_result in reader.lines() {
        let line = line_result.context("failed to read transcribe-server request line")?;
        if line.trim().is_empty() {
            // Blank lines are harmless — skip without responding so the
            // Python wrapper's response read doesn't see a phantom line.
            continue;
        }
        let response_json = encode_response_or_error(&line, &transcribe);
        writeln!(writer, "{response_json}")
            .context("failed to write transcribe-server response line")?;
        writer
            .flush()
            .context("failed to flush transcribe-server response line")?;
    }
    Ok(())
}

/// Render either a [`TranscribeResponse`] or an `{"error": "..."}` envelope
/// for one request line. Split out so the encoding (which is the only place
/// per-request error formatting lives) is unit-testable without spinning a
/// full reader/writer pair.
pub(crate) fn encode_response_or_error<F>(line: &str, transcribe: &F) -> String
where
    F: Fn(&str, Option<&str>, Option<&str>) -> Result<String>,
{
    let request: TranscribeRequest = match serde_json::from_str(line) {
        Ok(req) => req,
        Err(err) => {
            // Include the offending line so the Python side can log it,
            // truncated to keep an accidental megabyte of garbage out of
            // the response envelope.
            let snippet: String = line.chars().take(200).collect();
            return error_envelope(&format!(
                "failed to parse transcribe-server JSON request: {err}: {snippet}"
            ));
        }
    };
    match request {
        TranscribeRequest::TranscribeWav {
            wav_path,
            language,
            initial_prompt,
        } => {
            let lang = normalise_language(language.as_deref());
            let prompt = normalise_prompt(initial_prompt.as_deref());
            match transcribe(&wav_path, lang, prompt) {
                Ok(text) => serde_json::to_string(&TranscribeResponse { text })
                    .unwrap_or_else(|e| error_envelope(&format!("response serialise failed: {e}"))),
                Err(err) => error_envelope(&format!("{err:#}")),
            }
        }
    }
}

/// Build the per-request error envelope. The shape must stay stable
/// because the Python wrapper greps for the `error` key; if you change it,
/// update `vp_transcribe.py` in lockstep.
pub(crate) fn error_envelope(message: &str) -> String {
    serde_json::to_string(&serde_json::json!({ "error": message })).unwrap_or_else(|_| {
        // Last-resort fallback: hand-craft the JSON if serde itself fails
        // (it shouldn't — `message` is just a string), so the server never
        // emits a non-JSON line.
        format!(
            r#"{{"error":"serialise failed for message of len {}"}}"#,
            message.len()
        )
    })
}

#[cfg(test)]
mod tests;
