//! Byte-equivalence tests for the worker-event emitter.
//!
//! The byte goldens below were captured from the Python emitter
//! (`src/python/whisper_dictate/vp_events.py::_emit_worker_event`)
//! by running the equivalent
//! `json.dumps(..., ensure_ascii=True, sort_keys=True, separators=(",", ":"))`
//! call and pasting the output. Locking these as byte-literal goldens
//! catches drift (key order, whitespace, escape casing, surrogate pairs,
//! `None`-key omission, Python-style float exponents) before Wave 5
//! PR 2 starts routing production traffic through this emitter.
//!
//! Every test that exercises an emit_* function runs under
//! [`ENV_LOCK`] with `VOICEPI_WORKER_EVENTS=1` so the env-gate that
//! mirrors `_emit_worker_event` (see [`events::write_line`]) lets the
//! line through. The dedicated env-gate test toggles the variable to
//! verify both branches.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};

use serde_json::{json, Map, Value};

use super::events::*;
use crate::test_env_lock::ENV_LOCK;

// --- env-var helper -------------------------------------------------------

/// Process-scoped guard that sets `VOICEPI_WORKER_EVENTS` for the
/// duration of a single test and restores the original value on Drop.
/// Callers MUST hold [`ENV_LOCK`] for the guard's lifetime — see the
/// `test_env_lock` module docs for the soundness contract.
struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

// --- helpers --------------------------------------------------------------

fn emit_status_bytes(event: &StatusEvent) -> Vec<u8> {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut buf = Vec::new();
    emit_status(&mut buf, event).expect("emit_status writes to Vec");
    buf
}

fn emit_audio_bytes(event: &AudioEvent) -> Vec<u8> {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut buf = Vec::new();
    emit_audio(&mut buf, event).expect("emit_audio writes to Vec");
    buf
}

fn emit_utterance_bytes(payload: &Value) -> Vec<u8> {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut buf = Vec::new();
    emit_utterance(&mut buf, payload).expect("emit_utterance writes to Vec");
    buf
}

fn emit_error_bytes(message: &str, payload: &Value) -> Vec<u8> {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
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

// --- audio event (P2-1: parity with vp_capture.py::_emit_audio_level) -----

#[test]
fn emit_audio_byte_golden_matches_python() {
    // Captured from Python with:
    //   payload = {"event": "audio"}
    //   payload.update({k: v for k, v in {
    //       "state": "recording",
    //       "level": round(0.25, 3),
    //       "raw_dbfs": round(-12.5, 1),
    //       "peak": round(0.5, 3),
    //       "capture_backend": "sounddevice",
    //       "audio_device": "Yeti Classic",
    //       "capture_channels": 2,
    //   }.items() if v is not None})
    //   print("[worker-event] " + json.dumps(payload, ensure_ascii=True,
    //                                        sort_keys=True,
    //                                        separators=(",", ":")))
    let event = AudioEvent {
        level: 0.25,
        raw_dbfs: -12.5,
        peak: 0.5,
        capture_backend: Some("sounddevice".into()),
        audio_device: Some("Yeti Classic".into()),
        capture_channels: Some(2),
    };
    let bytes = emit_audio_bytes(&event);
    let expected: &[u8] = b"[worker-event] {\"audio_device\":\"Yeti Classic\",\"capture_backend\":\"sounddevice\",\"capture_channels\":2,\"event\":\"audio\",\"level\":0.25,\"peak\":0.5,\"raw_dbfs\":-12.5,\"state\":\"recording\"}\n";
    assert_eq!(bytes, expected);
}

#[test]
fn emit_audio_byte_golden_with_python_float_exponents() {
    // Captured from Python — exercises the float boundary cases (1e-6
    // round-trips as "1e-06", -100.0 stays fixed, 0.0001 stays fixed)
    // AND the Danish-diacritic path through the AsciiFormatter. The
    // device-fields-set-to-None branch is also covered: capture_backend
    // / audio_device populated, capture_channels=1.
    let event = AudioEvent {
        level: 0.000001,
        raw_dbfs: -100.0,
        peak: 0.0001,
        capture_backend: Some("sounddevice".into()),
        audio_device: Some("Mikrofon æøå".into()),
        capture_channels: Some(1),
    };
    let bytes = emit_audio_bytes(&event);
    let expected: &[u8] = b"[worker-event] {\"audio_device\":\"Mikrofon \\u00e6\\u00f8\\u00e5\",\"capture_backend\":\"sounddevice\",\"capture_channels\":1,\"event\":\"audio\",\"level\":1e-06,\"peak\":0.0001,\"raw_dbfs\":-100.0,\"state\":\"recording\"}\n";
    assert_eq!(bytes, expected);
}

#[test]
fn emit_audio_drops_optional_device_fields_when_none() {
    // Python's `_emit_worker_event` drops keys whose value is None
    // before serialising; the Rust emitter mirrors that for the three
    // optional fields on AudioEvent.
    let event = AudioEvent {
        level: 0.0,
        raw_dbfs: -120.0,
        peak: 0.0,
        capture_backend: None,
        audio_device: None,
        capture_channels: None,
    };
    let bytes = emit_audio_bytes(&event);
    let expected: &[u8] = b"[worker-event] {\"event\":\"audio\",\"level\":0.0,\"peak\":0.0,\"raw_dbfs\":-120.0,\"state\":\"recording\"}\n";
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

// --- canonical "event" key cannot be overridden (P2-4) --------------------

#[test]
fn emit_status_extras_event_key_does_not_override_canonical() {
    // Pre-fix, an extras map containing `"event"` would clobber the
    // emitter's `"event":"status"` insertion because the extras loop
    // ran AFTER the canonical insert and used unconditional `insert`.
    // `emit_utterance` / `emit_error` already protected this via
    // `write_named_event`; `emit_status` now does too.
    let mut extras = Map::new();
    extras.insert("event".into(), json!("not-status"));
    extras.insert("model".into(), json!("large-v3"));
    let event = StatusEvent {
        state: WorkerStatus::Ready,
        extras,
        ..StatusEvent::new(WorkerStatus::Ready)
    };
    let bytes = emit_status_bytes(&event);
    let body = strip_envelope(&bytes);
    // "event" stays "status"; "not-status" is dropped on the floor.
    assert_eq!(
        body,
        br#"{"event":"status","model":"large-v3","state":"ready"}"#
    );
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

// --- P2-2: DEL (U+007F) must escape ---------------------------------------

#[test]
fn del_control_character_escapes_as_u007f() {
    // Captured from Python:
    //   py -3 -c "import json; print(json.dumps('\x7f', ensure_ascii=True))"
    //   -> ""
    // Pre-fix, CompactFormatter let 0x7F through verbatim because its
    // printable-ASCII check uses 0x20..=0x7e (DEL is one past the end);
    // the AsciiFormatter now treats 0x7F like the non-ASCII branch.
    let payload = json!({"text": "boundary:\u{007F}<<"});
    let bytes = emit_utterance_bytes(&payload);
    let body = strip_envelope(&bytes);
    let expected: &[u8] = b"{\"event\":\"utterance\",\"text\":\"boundary:\\u007f<<\"}";
    assert_eq!(body, expected);
}

// --- wire-string stability for every status -------------------------------

#[test]
fn worker_status_wire_strings_match_python() {
    // Locks the exact strings the Python orchestrator emits across
    // vp_dictate.py + vp_capture.py + vp_preview.py + runtime.py. A
    // typo here would silently break the UI's status-switch ladder.
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
    assert_eq!(WorkerStatus::Preview.as_wire_str(), "preview");
    assert_eq!(WorkerStatus::CaptureLost.as_wire_str(), "capture_lost");
    assert_eq!(WorkerStatus::Ready.as_wire_str(), "ready");
}

#[test]
fn preview_and_capture_lost_round_trip_through_status_event() {
    // Mirror of the two states the Rust UI's app.rs switch ladder
    // recognises but the Rust enum was missing (P2-3): "preview" is the
    // live transcription update from vp_preview.py and "capture_lost"
    // is vp_capture.py's mid-recording device-failure signal.
    let preview = emit_status_bytes(&StatusEvent::new(WorkerStatus::Preview));
    assert_eq!(
        strip_envelope(&preview),
        br#"{"event":"status","state":"preview"}"#
    );
    let lost = emit_status_bytes(&StatusEvent::new(WorkerStatus::CaptureLost));
    assert_eq!(
        strip_envelope(&lost),
        br#"{"event":"status","state":"capture_lost"}"#
    );
}

// --- P2-5: Python-style float formatting (exponent padding + boundary) ----

#[test]
fn float_exponent_padding_matches_python_repr() {
    // Python pads scientific-notation exponents to >=2 digits with an
    // explicit sign: `1e-6` becomes `1e-06`, `1e+16` stays `1e+16`. The
    // boundary fixed/scientific switch also differs from serde_json:
    // Python uses scientific when |x| < 1e-4 even if serde_json would
    // emit fixed-point ("0.00001" becomes "1e-05").
    let payload = json!({
        "small_exp":     0.000001_f64, // Python "1e-06"; serde_json "1e-6"
        "smaller_exp":   1e-9_f64,     // Python "1e-09"
        "fractional":    1.5e-6_f64,   // Python "1.5e-06"
        "negative":      -1e-6_f64,    // Python "-1e-06"
        "boundary_low":  1e-5_f64,     // Python "1e-05"; serde_json "0.00001"
        "boundary_mid":  1e-4_f64,     // Python "0.0001"; serde_json "0.0001"
        "boundary_25":   2.5e-5_f64,   // Python "2.5e-05"; serde_json "0.000025"
        "boundary_high": 1e16_f64,     // Python "1e+16"; serde_json "1e+16"
        "boundary_1e15": 1e15_f64,     // Python "1000000000000000.0" (fixed)
        "regular":       0.25_f64,
        "negative_neg0": -0.0_f64,     // Python "-0.0"
        "huge":          1.5e16_f64,   // Python "1.5e+16"
        "tiny_neg":      -1.5e-7_f64,  // Python "-1.5e-07"
    });
    let bytes = emit_utterance_bytes(&payload);
    let expected: &[u8] = b"[worker-event] {\
\"boundary_1e15\":1000000000000000.0,\
\"boundary_25\":2.5e-05,\
\"boundary_high\":1e+16,\
\"boundary_low\":1e-05,\
\"boundary_mid\":0.0001,\
\"event\":\"utterance\",\
\"fractional\":1.5e-06,\
\"huge\":1.5e+16,\
\"negative\":-1e-06,\
\"negative_neg0\":-0.0,\
\"regular\":0.25,\
\"small_exp\":1e-06,\
\"smaller_exp\":1e-09,\
\"tiny_neg\":-1.5e-07\
}\n";
    assert_eq!(
        bytes, expected,
        "AsciiFormatter::write_f64 must reproduce CPython's repr(float)"
    );
}

#[test]
fn float_positive_zero_emits_plain_zero() {
    // Python json.dumps(0.0) -> "0.0"; -0.0 is covered by the panel
    // above. A standalone test isolates the +0.0 short-circuit branch.
    let payload = json!({"v": 0.0_f64});
    let bytes = emit_utterance_bytes(&payload);
    assert_eq!(strip_envelope(&bytes), br#"{"event":"utterance","v":0.0}"#);
}

// --- VOICEPI_WORKER_EVENTS env-gate (P2-6) --------------------------------

#[test]
fn worker_events_env_var_gates_emission() {
    // Mirrors `vp_events.py::_emit_worker_event`'s short-circuit:
    //   if not _truthy(os.environ.get("VOICEPI_WORKER_EVENTS")): return
    // The Rust emitter applies the same _truthy rules from
    // `crate::dictate::env_gates::is_truthy`, so "", "0", "false",
    // "no", "off" (case-insensitive, trimmed) suppress the write while
    // anything else lets it through.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Off by default — variable not set => no-op.
    {
        let _env = EnvVarGuard::remove("VOICEPI_WORKER_EVENTS");
        let mut buf = Vec::new();
        emit_status(&mut buf, &StatusEvent::new(WorkerStatus::Ready)).unwrap();
        assert!(buf.is_empty(), "unset env => no write, got {buf:?}");
    }

    // Each of Python's falsy strings (and a case/whitespace variant)
    // suppress the write.
    for falsy in ["", "0", "false", "FALSE", " False ", "no", "off"] {
        let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", falsy);
        let mut buf = Vec::new();
        emit_status(&mut buf, &StatusEvent::new(WorkerStatus::Ready)).unwrap();
        assert!(
            buf.is_empty(),
            "VOICEPI_WORKER_EVENTS={falsy:?} should suppress, got {buf:?}"
        );
    }

    // Truthy values let the line through. Cover the three common
    // Python idioms (1 / true / yes / on) plus a free-form one.
    for truthy in ["1", "true", "yes", "on", "anything-else"] {
        let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", truthy);
        let mut buf = Vec::new();
        emit_status(&mut buf, &StatusEvent::new(WorkerStatus::Ready)).unwrap();
        let body = std::str::from_utf8(&buf).unwrap();
        assert!(
            body.starts_with(WORKER_EVENT_PREFIX) && body.ends_with('\n'),
            "VOICEPI_WORKER_EVENTS={truthy:?} should emit, got {body:?}"
        );
    }
}

// --- P2-7: flush after newline --------------------------------------------

/// Writer that tracks the number of `flush()` calls and the bytes
/// written so a single test can verify both the wire output AND the
/// flush semantics in one go.
struct FlushTrackingWriter {
    buf: Vec<u8>,
    flush_count: usize,
}

impl Write for FlushTrackingWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.write(data)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flush_count += 1;
        Ok(())
    }
}

#[test]
fn write_line_flushes_once_per_event() {
    // Python uses `print(..., flush=True)`, so each worker-event line
    // is fully observable to a downstream reader the moment it lands.
    // The Rust emitter does the same: `write_line` flushes after the
    // terminating newline.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");

    let mut writer = FlushTrackingWriter {
        buf: Vec::new(),
        flush_count: 0,
    };
    emit_status(&mut writer, &StatusEvent::new(WorkerStatus::Ready)).unwrap();
    assert_eq!(
        writer.flush_count, 1,
        "one emit_status call should produce exactly one flush"
    );

    // Two more events should bring the flush count to three.
    emit_audio(
        &mut writer,
        &AudioEvent {
            level: 0.1,
            raw_dbfs: -20.0,
            peak: 0.2,
            ..AudioEvent::default()
        },
    )
    .unwrap();
    emit_utterance(&mut writer, &json!({"text": "x"})).unwrap();
    assert_eq!(writer.flush_count, 3, "one flush per emit call");
}

// --- AsciiFormatter scalar coverage ---------------------------------------

#[test]
fn ascii_formatter_passes_through_every_json_scalar_kind() {
    // The custom AsciiFormatter delegates every method except
    // `write_string_fragment` and the float overrides to serde_json's
    // CompactFormatter, but each delegation still needs at least one
    // byte-equality assertion so SonarCloud sees the path covered
    // (otherwise the new-code coverage gate dips below 80% on this PR).
    // Encode every JSON scalar shape that maps to a distinct Formatter
    // method (`write_bool`, signed/unsigned int widths, `write_f64`),
    // run them through the emitter via the utterance entry point, and
    // check the bytes match the Python emitter's output.
    // NOTE: `emit_utterance` filters `null`-valued payload keys to
    // match Python's `if v is not None` shape, so we don't include a
    // null here — the null-drop behaviour is locked by
    // `null_utterance_keys_are_dropped`.
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
    // `begin_object` / `begin_object_key` / `end_object` delegations
    // that a flat scalar payload doesn't reach.
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
