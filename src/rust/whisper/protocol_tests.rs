//! Unit tests for [`super`] (`whisper::protocol`).
//!
//! Kept in a sibling file (`protocol_tests.rs`) so `protocol.rs` stays under
//! the repo's 500-LOC modularity gate (AGENTS.md). Declared via
//! `#[cfg(test)] #[path = "protocol_tests.rs"] mod tests;` — identical to the
//! `wav.rs` / `wav_tests.rs` convention already in this module.

use super::*;

/// Extract the `text` field from a success response line.
///
/// Responses grew an `accel` provenance field, so byte-exact JSON
/// comparisons would (a) break on every future additive field and (b)
/// depend on the process-wide accelerator observer, which other tests
/// mutate. Assert on the field these tests are actually about.
fn response_text(line: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON: {line}: {e}"));
    parsed["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no string `text` field in {line}"))
        .to_owned()
}

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
    assert_eq!(normalise_language(Some("Auto")), None);
    assert_eq!(normalise_language(Some("AUTO")), None);
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

/// A single recorded transcribe invocation: `(wav_path, language, prompt)`.
/// Pulled out as a type alias so the nested generics on `FakeTranscribe`
/// don't trip `clippy::type_complexity` on the windows-clippy lint step
/// (which runs `--tests`, unlike the default `cargo clippy` invocation).
type RecordedCall = (String, Option<String>, Option<String>);

/// Recording fake: counts calls and returns canned text per call so the
/// loop's request-routing can be tested without whisper.cpp.
struct FakeTranscribe {
    calls: std::sync::Mutex<Vec<RecordedCall>>,
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
    fn calls(&self) -> Vec<RecordedCall> {
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
    assert_eq!(response_text(&json), "hello world");
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
    assert_eq!(response_text(lines[0]), "first");
    assert_eq!(response_text(lines[1]), "second");
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
    assert_eq!(response_text(lines[0]), "only");
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
    assert_eq!(response_text(lines[1]), "recovered");
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
    assert_eq!(response_text(&lines[1]), "recovered");
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

// -- response provenance (`accel`) ------------------------------------

#[test]
fn transcribe_response_carries_the_observed_accelerator() {
    // The Python worker pipes this helper's stderr to DEVNULL, so the
    // response envelope is the ONLY way whisper.cpp's GPU verdict can
    // reach the utterance record. Without this field a Vulkan-linked
    // helper that fell back to CPU is indistinguishable from one that
    // did not.
    let _guard = crate::test_env_lock::ACCEL_OBSERVER_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let observer = crate::whisper::accel::global();
    observer.reset();

    let unknown = TranscribeResponse::new("hi".to_owned());
    assert_eq!(unknown.accel, "unknown", "no observation yet");

    observer.note_log_line("whisper_backend_init_gpu: using Vulkan0 backend");
    let seen = TranscribeResponse::new("hi".to_owned());
    assert_eq!(seen.accel, "vulkan");
    let json: serde_json::Value = serde_json::to_value(&seen).unwrap();
    assert_eq!(json["text"], serde_json::json!("hi"));
    assert_eq!(json["accel"], serde_json::json!("vulkan"));

    observer.reset();
}

#[test]
fn encode_response_stamps_accel_on_the_server_wire() {
    // Same contract, but through the actual server encoder the Python
    // wrapper reads -- so a future refactor that bypasses
    // `TranscribeResponse::new` is caught.
    let _guard = crate::test_env_lock::ACCEL_OBSERVER_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let observer = crate::whisper::accel::global();
    observer.reset();
    observer.note_log_line("whisper_backend_init_gpu: no GPU found");

    let fake = FakeTranscribe::new();
    fake.push_ok("hello");
    let json = encode_response_or_error(
        r#"{"action":"transcribe_wav","wav_path":"/tmp/x.wav"}"#,
        &fake.make_closure(),
    );
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["accel"], serde_json::json!("cpu"));

    observer.reset();
}
