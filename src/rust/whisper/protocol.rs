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
mod tests {
    use super::*;

    // -- request parsing -------------------------------------------------

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

    // -- normalisation ---------------------------------------------------

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

    // -- serve_loop + encode_response_or_error ---------------------------

    /// Recording fake: counts calls and returns canned text per call so the
    /// loop's request-routing can be tested without whisper.cpp.
    struct FakeTranscribe {
        calls: std::sync::Mutex<Vec<(String, Option<String>, Option<String>)>>,
        next_response: std::sync::Mutex<std::collections::VecDeque<Result<String>>>,
    }

    impl FakeTranscribe {
        fn new() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                next_response: std::sync::Mutex::new(std::collections::VecDeque::new()),
            }
        }
        fn push_ok(&self, text: &str) {
            self.next_response
                .lock()
                .unwrap()
                .push_back(Ok(text.to_owned()));
        }
        fn push_err(&self, err: &str) {
            self.next_response
                .lock()
                .unwrap()
                .push_back(Err(anyhow::anyhow!("{err}")));
        }
        fn calls(&self) -> Vec<(String, Option<String>, Option<String>)> {
            self.calls.lock().unwrap().clone()
        }
        fn make_closure(&self) -> impl Fn(&str, Option<&str>, Option<&str>) -> Result<String> + '_ {
            move |wav, lang, prompt| {
                self.calls.lock().unwrap().push((
                    wav.to_owned(),
                    lang.map(str::to_owned),
                    prompt.map(str::to_owned),
                ));
                self.next_response
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| Err(anyhow::anyhow!("test fixture exhausted")))
            }
        }
    }

    #[test]
    fn encode_response_serialises_text_on_success() {
        let fake = FakeTranscribe::new();
        fake.push_ok("hello world");
        let json = encode_response_or_error(
            r#"{"action":"transcribe_wav","wav_path":"/tmp/x.wav"}"#,
            &fake.make_closure(),
        );
        assert_eq!(json, r#"{"text":"hello world"}"#);
    }

    #[test]
    fn encode_response_normalises_language_and_prompt_before_invoking_transcribe() {
        let fake = FakeTranscribe::new();
        fake.push_ok("ok");
        let _ = encode_response_or_error(
            r#"{"action":"transcribe_wav","wav_path":"/x.wav","language":"auto","initial_prompt":""}"#,
            &fake.make_closure(),
        );
        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, None, "language=auto must collapse to None");
        assert_eq!(calls[0].2, None, "empty prompt must collapse to None");
    }

    #[test]
    fn encode_response_passes_explicit_language_through() {
        let fake = FakeTranscribe::new();
        fake.push_ok("ok");
        let _ = encode_response_or_error(
            r#"{"action":"transcribe_wav","wav_path":"/x.wav","language":"da"}"#,
            &fake.make_closure(),
        );
        assert_eq!(fake.calls()[0].1, Some("da".to_owned()));
    }

    #[test]
    fn encode_response_emits_error_envelope_on_transcribe_failure() {
        let fake = FakeTranscribe::new();
        fake.push_err("model blew up");
        let json = encode_response_or_error(
            r#"{"action":"transcribe_wav","wav_path":"/tmp/x.wav"}"#,
            &fake.make_closure(),
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            parsed.get("error").is_some(),
            "expected error envelope, got {json}"
        );
        assert!(parsed["error"].as_str().unwrap().contains("model blew up"));
        // And NOT a success-shape so the Python wrapper can rely on the
        // `error` vs `text` key to distinguish.
        assert!(parsed.get("text").is_none());
    }

    #[test]
    fn encode_response_emits_error_envelope_on_parse_failure() {
        let fake = FakeTranscribe::new();
        let json = encode_response_or_error("not json at all", &fake.make_closure());
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("error").is_some(), "got: {json}");
        // The fake closure must NOT have been called for a parse failure.
        assert_eq!(fake.calls().len(), 0);
    }

    #[test]
    fn encode_response_truncates_overly_long_garbage_in_parse_error() {
        // A multi-MB garbage line should not produce a multi-MB error
        // envelope — that would defeat the per-request error contract.
        let fake = FakeTranscribe::new();
        let huge = "x".repeat(10_000);
        let json = encode_response_or_error(&huge, &fake.make_closure());
        assert!(
            json.len() < 500,
            "error envelope should truncate the offending line; got {} bytes",
            json.len()
        );
    }

    #[test]
    fn serve_loop_round_trips_two_requests_and_keeps_model_loaded() {
        // The core promise of `transcribe-server`: one process, many
        // requests. A single fake closure handles every call so any
        // "reload the model between calls" bug would surface as either
        // extra calls counted or a failed second response.
        let fake = FakeTranscribe::new();
        fake.push_ok("first");
        fake.push_ok("second");
        let input = concat!(
            r#"{"action":"transcribe_wav","wav_path":"/a.wav"}"#,
            "\n",
            r#"{"action":"transcribe_wav","wav_path":"/b.wav"}"#,
            "\n",
        );
        let mut output = Vec::new();
        serve_loop(input.as_bytes(), &mut output, fake.make_closure()).unwrap();
        let out = String::from_utf8(output).unwrap();
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines.len(), 2, "got {lines:?}");
        assert_eq!(lines[0], r#"{"text":"first"}"#);
        assert_eq!(lines[1], r#"{"text":"second"}"#);
        // Exactly two transcribe calls — no spurious reloads.
        assert_eq!(fake.calls().len(), 2);
        assert_eq!(fake.calls()[0].0, "/a.wav");
        assert_eq!(fake.calls()[1].0, "/b.wav");
    }

    #[test]
    fn serve_loop_skips_blank_lines_without_emitting_response() {
        let fake = FakeTranscribe::new();
        fake.push_ok("only");
        let input = "\n   \n\t\n{\"action\":\"transcribe_wav\",\"wav_path\":\"/x.wav\"}\n\n";
        let mut output = Vec::new();
        serve_loop(input.as_bytes(), &mut output, fake.make_closure()).unwrap();
        let out = String::from_utf8(output).unwrap();
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines.len(), 1, "blank lines should produce no response");
        assert_eq!(lines[0], r#"{"text":"only"}"#);
    }

    #[test]
    fn serve_loop_continues_after_per_request_error() {
        // The headline contract for the long-running server: a single bad
        // request must not tear down the worker. The first call errors,
        // the second succeeds, both responses must reach the writer.
        let fake = FakeTranscribe::new();
        fake.push_err("boom");
        fake.push_ok("recovered");
        let input = concat!(
            r#"{"action":"transcribe_wav","wav_path":"/bad.wav"}"#,
            "\n",
            r#"{"action":"transcribe_wav","wav_path":"/good.wav"}"#,
            "\n",
        );
        let mut output = Vec::new();
        serve_loop(input.as_bytes(), &mut output, fake.make_closure()).unwrap();
        let out = String::from_utf8(output).unwrap();
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert!(first["error"].as_str().unwrap().contains("boom"));
        assert_eq!(lines[1], r#"{"text":"recovered"}"#);
    }

    #[test]
    fn serve_loop_continues_after_parse_error() {
        let fake = FakeTranscribe::new();
        fake.push_ok("recovered");
        let input = concat!(
            "definitely not json\n",
            r#"{"action":"transcribe_wav","wav_path":"/ok.wav"}"#,
            "\n",
        );
        let mut output = Vec::new();
        serve_loop(input.as_bytes(), &mut output, fake.make_closure()).unwrap();
        let lines: Vec<_> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert!(
            first["error"].is_string(),
            "first line should be error: {}",
            lines[0]
        );
        assert_eq!(lines[1], r#"{"text":"recovered"}"#);
    }

    #[test]
    fn serve_loop_returns_ok_on_clean_eof() {
        let fake = FakeTranscribe::new();
        let mut output = Vec::new();
        // Empty reader → clean EOF immediately.
        serve_loop(b"".as_slice(), &mut output, fake.make_closure()).unwrap();
        assert!(output.is_empty());
        assert_eq!(fake.calls().len(), 0);
    }

    #[test]
    fn error_envelope_uses_stable_shape() {
        // The Python wrapper greps for the `error` key — changing the
        // shape silently would break the wrapper. This test pins the
        // contract so any rename surfaces as a failing assertion.
        let env = error_envelope("something broke");
        let parsed: serde_json::Value = serde_json::from_str(&env).unwrap();
        assert_eq!(parsed["error"].as_str(), Some("something broke"));
        assert_eq!(parsed.as_object().unwrap().len(), 1, "{env}");
    }

    #[test]
    fn server_ready_serialises_to_documented_shape() {
        let ready = ServerReady {
            ready: true,
            model_path: "/tmp/ggml-tiny.en.bin".to_owned(),
            idle_unload_s: 300,
        };
        let json: serde_json::Value = serde_json::to_value(&ready).unwrap();
        assert_eq!(json["ready"], serde_json::json!(true));
        assert_eq!(
            json["model_path"],
            serde_json::json!("/tmp/ggml-tiny.en.bin")
        );
        assert_eq!(json["idle_unload_s"], serde_json::json!(300));
    }
}
