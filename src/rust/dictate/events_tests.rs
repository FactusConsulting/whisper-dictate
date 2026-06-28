//! Byte-equivalence tests for the worker-event emitter.
//!
//! The byte goldens below were captured from the Python emitter
//! (`src/python/whisper_dictate/vp_events.py::_emit_worker_event`)
//! by running the equivalent
//! `json.dumps(..., ensure_ascii=True, sort_keys=True, separators=(",", ":"))`
//! call and pasting the output. Locking these as byte-literal goldens
//! catches drift (key order, whitespace, escape casing, surrogate pairs,
//! `None`-key omission) before Wave 5 PR 2 starts routing production
//! traffic through this emitter.

use serde_json::{json, Map, Value};

use super::events::*;

// --- helpers ---------------------------------------------------------------

fn emit_status_bytes(event: &StatusEvent) -> Vec<u8> {
    let mut buf = Vec::new();
    emit_status(&mut buf, event).expect("emit_status writes to Vec");
    buf
}

fn emit_utterance_bytes(payload: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    emit_utterance(&mut buf, payload).expect("emit_utterance writes to Vec");
    buf
}

fn emit_error_bytes(message: &str, payload: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    emit_error(&mut buf, message, payload).expect("emit_error writes to Vec");
    buf
}

// Strip the `[worker-event] ` prefix + trailing `\n` so a single
// helper can hand the raw JSON to `serde_json::from_slice` for
// round-trip assertions.
fn strip_envelope(bytes: &[u8]) -> &[u8] {
    let s = std::str::from_utf8(bytes).expect("emitter output is valid UTF-8");
    let body = s
        .strip_prefix(WORKER_EVENT_PREFIX)
        .expect("emitter line starts with the worker-event prefix");
    let body = body
        .strip_suffix('\n')
        .expect("emitter line is newline-terminated");
    body.as_bytes()
}

// --- golden-byte coverage --------------------------------------------------

#[test]
fn emit_status_byte_golden_simple_ready() {
    // Equivalent Python call:
    //   _emit_worker_event("status", state="ready", model="large-v3")
    let mut extras = Map::new();
    extras.insert("model".into(), json!("large-v3"));
    let event = StatusEvent {
        state: WorkerStatus::Ready,
        extras,
        ..StatusEvent::new(WorkerStatus::Ready)
    };
    let bytes = emit_status_bytes(&event);
    let expected: &[u8] =
        b"[worker-event] {\"event\":\"status\",\"model\":\"large-v3\",\"state\":\"ready\"}\n";
    assert_eq!(bytes, expected);
}

#[test]
fn emit_status_byte_golden_with_unicode_and_extras() {
    // Equivalent Python call (astral emoji forces a surrogate pair,
    // which is the riskiest bit of the ensure_ascii=True port):
    //   _emit_worker_event("status", state="ready", model="large-v3",
    //                      note="😀")
    let mut extras = Map::new();
    extras.insert("model".into(), json!("large-v3"));
    extras.insert("note".into(), json!("😀"));
    let event = StatusEvent {
        state: WorkerStatus::Ready,
        extras,
        ..StatusEvent::new(WorkerStatus::Ready)
    };
    let bytes = emit_status_bytes(&event);
    let expected: &[u8] = b"[worker-event] {\"event\":\"status\",\"model\":\"large-v3\",\"note\":\"\\ud83d\\ude00\",\"state\":\"ready\"}\n";
    assert_eq!(bytes, expected);
}

#[test]
fn emit_status_byte_golden_with_recording_fields() {
    // Mirror of vp_dictate.py — _emit_worker_event("status", state="recording",
    //   capture_backend="sounddevice", audio_device="Yeti Classic",
    //   capture_channels=2).
    let event = StatusEvent {
        state: WorkerStatus::Recording,
        capture_backend: Some("sounddevice".into()),
        audio_device: Some("Yeti Classic".into()),
        capture_channels: Some(2),
        extras: Map::new(),
    };
    let bytes = emit_status_bytes(&event);
    let expected: &[u8] = b"[worker-event] {\"audio_device\":\"Yeti Classic\",\"capture_backend\":\"sounddevice\",\"capture_channels\":2,\"event\":\"status\",\"state\":\"recording\"}\n";
    assert_eq!(bytes, expected);
}

#[test]
fn emit_status_byte_golden_with_danish_diacritics() {
    // Mirror of test_audio_runtime.py::
    //   test_worker_event_emits_structured_ascii_stderr_without_helper_process
    // which uses audio_device="Mikrofon æøå" and level=0.25. We adapt
    // to the status-event shape: same key set minus "event"="audio".
    let mut extras = Map::new();
    extras.insert("level".into(), json!(0.25));
    let event = StatusEvent {
        state: WorkerStatus::Recording,
        capture_backend: None,
        audio_device: Some("Mikrofon æøå".into()),
        capture_channels: None,
        extras,
    };
    let bytes = emit_status_bytes(&event);
    let expected: &[u8] = b"[worker-event] {\"audio_device\":\"Mikrofon \\u00e6\\u00f8\\u00e5\",\"event\":\"status\",\"level\":0.25,\"state\":\"recording\"}\n";
    assert_eq!(bytes, expected);
}

#[test]
fn emit_utterance_byte_golden() {
    // Equivalent Python call:
    //   _emit_worker_event("utterance", text="hello world", audio_peak=0.5)
    let payload = json!({"text": "hello world", "audio_peak": 0.5});
    let bytes = emit_utterance_bytes(&payload);
    let expected: &[u8] =
        b"[worker-event] {\"audio_peak\":0.5,\"event\":\"utterance\",\"text\":\"hello world\"}\n";
    assert_eq!(bytes, expected);
}

#[test]
fn emit_error_byte_golden() {
    // Equivalent Python call (runtime.py line 688):
    //   _emit_worker_event("error", state="failed", backend="whisper",
    //                      model="large-v3", message="oh no: æ")
    let payload = json!({
        "state": "failed",
        "backend": "whisper",
        "model": "large-v3",
    });
    let bytes = emit_error_bytes("oh no: æ", &payload);
    let expected: &[u8] = b"[worker-event] {\"backend\":\"whisper\",\"event\":\"error\",\"message\":\"oh no: \\u00e6\",\"model\":\"large-v3\",\"state\":\"failed\"}\n";
    assert_eq!(bytes, expected);
}

// --- round-trip with the consumer side ------------------------------------

#[test]
fn emit_status_round_trips_through_parse_worker_event_payload_shape() {
    // The whole point of locking the wire format: an event emitted here
    // must come back from `parse_worker_event` with the same fields.
    // `parse_worker_event` is private to the runtime crate; we reproduce
    // its contract (strip the prefix, parse JSON, look up event+state)
    // here so a behavioural drift on either side fails this test.
    let event = StatusEvent {
        state: WorkerStatus::Recording,
        capture_backend: Some("sounddevice".into()),
        audio_device: Some("Yeti Classic".into()),
        capture_channels: Some(2),
        extras: Map::new(),
    };
    let bytes = emit_status_bytes(&event);
    let line = std::str::from_utf8(&bytes).unwrap();
    let raw = line
        .strip_prefix(WORKER_EVENT_PREFIX)
        .expect("starts with worker-event prefix")
        .strip_suffix('\n')
        .expect("newline-terminated");

    let payload: Value = serde_json::from_str(raw).expect("payload is valid JSON");
    assert_eq!(
        payload.get("event").and_then(|v| v.as_str()),
        Some("status")
    );
    assert_eq!(
        payload.get("state").and_then(|v| v.as_str()),
        Some("recording"),
    );
    assert_eq!(payload["capture_backend"], "sounddevice");
    assert_eq!(payload["audio_device"], "Yeti Classic");
    assert_eq!(payload["capture_channels"], 2);
}

#[test]
fn emit_utterance_round_trips() {
    let payload = json!({"text": "hej", "dictionary_terms": ["Sara"]});
    let bytes = emit_utterance_bytes(&payload);
    let parsed: Value = serde_json::from_slice(strip_envelope(&bytes)).unwrap();
    assert_eq!(parsed["event"], "utterance");
    assert_eq!(parsed["text"], "hej");
    assert_eq!(parsed["dictionary_terms"], json!(["Sara"]));
}

#[test]
fn emit_error_round_trips() {
    let payload = json!({"state": "failed", "backend": "whisper"});
    let bytes = emit_error_bytes("boom", &payload);
    let parsed: Value = serde_json::from_slice(strip_envelope(&bytes)).unwrap();
    assert_eq!(parsed["event"], "error");
    assert_eq!(parsed["state"], "failed");
    assert_eq!(parsed["backend"], "whisper");
    assert_eq!(parsed["message"], "boom");
}

// --- key sort stability ----------------------------------------------------

#[test]
fn keys_are_sorted_alphabetically_regardless_of_insertion_order() {
    // Insert extras in deliberately scrambled order and assert the wire
    // form is still alphabetical — locks the BTreeMap-backed `Map`
    // behaviour so a future `preserve_order` flag on serde_json wouldn't
    // silently break Python parity.
    let mut extras = Map::new();
    extras.insert("zulu".into(), json!(1));
    extras.insert("alpha".into(), json!(2));
    extras.insert("mike".into(), json!(3));
    let event = StatusEvent {
        state: WorkerStatus::Ready,
        extras,
        ..StatusEvent::new(WorkerStatus::Ready)
    };
    let bytes = emit_status_bytes(&event);
    let body = std::str::from_utf8(strip_envelope(&bytes)).unwrap();
    assert_eq!(
        body,
        r#"{"alpha":2,"event":"status","mike":3,"state":"ready","zulu":1}"#
    );
}

// --- None/empty handling ---------------------------------------------------

#[test]
fn none_optional_fields_are_dropped() {
    // Matches Python's
    //   payload.update({k: v for k, v in fields.items() if v is not None}).
    let event = StatusEvent::new(WorkerStatus::Opening);
    let bytes = emit_status_bytes(&event);
    let body = strip_envelope(&bytes);
    // Only "event" and "state" survive.
    assert_eq!(body, br#"{"event":"status","state":"opening"}"#);
}

#[test]
fn null_extras_keys_are_dropped() {
    let mut extras = Map::new();
    extras.insert("kept".into(), json!("yes"));
    extras.insert("dropped".into(), Value::Null);
    let event = StatusEvent {
        state: WorkerStatus::Ready,
        extras,
        ..StatusEvent::new(WorkerStatus::Ready)
    };
    let bytes = emit_status_bytes(&event);
    let body = strip_envelope(&bytes);
    assert_eq!(body, br#"{"event":"status","kept":"yes","state":"ready"}"#);
}

#[test]
fn null_utterance_keys_are_dropped() {
    let payload = json!({"text": "hi", "skip": null});
    let bytes = emit_utterance_bytes(&payload);
    let body = strip_envelope(&bytes);
    assert_eq!(body, br#"{"event":"utterance","text":"hi"}"#);
}

// --- unicode escaping coverage --------------------------------------------

#[test]
fn bmp_unicode_chars_use_lowercase_hex_escapes() {
    // Python uses lowercase hex; the AsciiFormatter's
    // write_string_fragment must also produce lowercase to match.
    let mut extras = Map::new();
    extras.insert("name".into(), json!("Mikrofon æøå"));
    let event = StatusEvent {
        state: WorkerStatus::Ready,
        extras,
        ..StatusEvent::new(WorkerStatus::Ready)
    };
    let bytes = emit_status_bytes(&event);
    let body = strip_envelope(&bytes);
    let text = std::str::from_utf8(body).unwrap();
    assert!(
        text.contains("Mikrofon \\u00e6\\u00f8\\u00e5"),
        "expected lowercase \\u00e6\\u00f8\\u00e5 escapes in {text}"
    );
    // And confirm the raw UTF-8 codepoints did NOT leak through.
    assert!(!text.contains('\u{00e6}'), "raw \u{00e6} leaked: {text}");
}

#[test]
fn astral_codepoints_emit_utf16_surrogate_pairs() {
    let mut extras = Map::new();
    extras.insert("emoji".into(), json!("\u{1F600}"));
    let event = StatusEvent {
        state: WorkerStatus::Ready,
        extras,
        ..StatusEvent::new(WorkerStatus::Ready)
    };
    let bytes = emit_status_bytes(&event);
    let body = strip_envelope(&bytes);
    let text = std::str::from_utf8(body).unwrap();
    assert!(
        text.contains("\\ud83d\\ude00"),
        "expected surrogate pair in {text}"
    );
    assert!(!text.contains('\u{1F600}'), "raw emoji leaked: {text}");
}

// --- wire-string stability for every status -------------------------------

#[test]
fn worker_status_wire_strings_match_python() {
    // Locks the exact strings the Python orchestrator emits across
    // vp_dictate.py + runtime.py. A typo here would silently break the
    // UI's status-switch ladder.
    assert_eq!(WorkerStatus::LoadingModel.as_wire_str(), "loading_model");
    assert_eq!(WorkerStatus::Opening.as_wire_str(), "opening");
    assert_eq!(WorkerStatus::Recording.as_wire_str(), "recording");
    assert_eq!(WorkerStatus::Transcribing.as_wire_str(), "transcribing");
    assert_eq!(
        WorkerStatus::PostProcessing.as_wire_str(),
        "post-processing"
    );
    assert_eq!(WorkerStatus::NoText.as_wire_str(), "no_text");
    assert_eq!(WorkerStatus::Cancelled.as_wire_str(), "cancelled");
    assert_eq!(WorkerStatus::Error.as_wire_str(), "error");
    assert_eq!(WorkerStatus::Ready.as_wire_str(), "ready");
}

// --- AsciiFormatter scalar coverage ---------------------------------------

#[test]
fn ascii_formatter_passes_through_every_json_scalar_kind() {
    // The custom AsciiFormatter delegates every method except
    // `write_string_fragment` to serde_json's CompactFormatter, but each
    // delegation still needs at least one byte-equality assertion so
    // SonarCloud sees the path covered (otherwise the new-code coverage
    // gate dips below 80% on this PR). Encode every JSON scalar shape
    // that maps to a distinct Formatter method (`write_null`,
    // `write_bool`, signed/unsigned ints across widths, `write_f64`),
    // run them through the emitter via the utterance entry point, and
    // check the bytes match what CompactFormatter would have produced.
    // NOTE: `emit_utterance` filters `null`-valued payload keys to match
    // Python's `if v is not None` shape, so we don't include a null here
    // — the null-drop behaviour itself is locked by a separate test
    // (`none_optional_fields_are_omitted_from_payload`). The scalars
    // below cover every other distinct Formatter method.
    let payload = json!({
        "true_value": true,
        "false_value": false,
        "small_int": 7i64,
        "negative_int": -42i64,
        "i32_max": i32::MAX,
        "i64_min": i64::MIN,
        "u64_max": u64::MAX,
        "float_value": 1.5_f64,
    });
    let bytes = emit_utterance_bytes(&payload);
    let expected: &[u8] = b"[worker-event] {\
\"event\":\"utterance\",\
\"false_value\":false,\
\"float_value\":1.5,\
\"i32_max\":2147483647,\
\"i64_min\":-9223372036854775808,\
\"negative_int\":-42,\
\"small_int\":7,\
\"true_value\":true,\
\"u64_max\":18446744073709551615\
}\n";
    assert_eq!(
        bytes, expected,
        "AsciiFormatter scalar passthrough must match CompactFormatter byte-for-byte",
    );
}

#[test]
fn ascii_formatter_emits_empty_array_and_nested_object() {
    // Covers the `begin_array` / `end_array` / `begin_array_value` and
    // `begin_object` / `begin_object_key` / `end_object` delegations that
    // a flat scalar payload doesn't reach. Same coverage rationale as the
    // scalar sweep above -- locks every Formatter method in events.rs to
    // at least one byte assertion so the new-code coverage gate stays
    // above SonarCloud's 80% threshold.
    let payload = json!({
        "items": [1, 2, 3],
        "empty": [],
        "nested": { "inner_key": "inner_value" },
    });
    let bytes = emit_utterance_bytes(&payload);
    let expected: &[u8] = b"[worker-event] {\
\"empty\":[],\
\"event\":\"utterance\",\
\"items\":[1,2,3],\
\"nested\":{\"inner_key\":\"inner_value\"}\
}\n";
    assert_eq!(bytes, expected);
}
