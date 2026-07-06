//! Worker-event line emitters used by [`super::DictateSession`].
//!
//! Wave 5 PR 1 (#412) introduces a richer [`crate::dictate::events`]
//! module with full Python `ensure_ascii=True` parity; this helper
//! writes the narrow `[worker-event] {…}\n` lines the session needs
//! (state strings + reason tokens — all ASCII) inline so this PR builds
//! standalone from `main` without depending on PR 1's merge order. PR 3
//! swaps these calls for `events::emit_status` / `events::emit_utterance`
//! once both PRs are in `main`.

use std::io::Write;

use serde_json::{Map, Value};

use super::types::{SessionError, TranscribeResult};

/// Stderr prefix every `[worker-event]` line carries. Matches
/// `runtime::WORKER_EVENT_PREFIX` so the existing
/// `runtime::parse_worker_event` consumer keeps working.
const WORKER_EVENT_PREFIX: &str = "[worker-event] ";

/// Emit one `[worker-event] {…,"event":"status",…}` line. Null /
/// empty-string optional fields are dropped to match the Python
/// emitter's `if v is not None` filter (extras here carry `""` as the
/// "no value" sentinel for capture_backend / audio_device, matching the
/// default `SessionConfig`).
pub(super) fn emit_status<W: Write>(
    writer: &mut W,
    state: &str,
    extras: &[(&str, Value)],
) -> Result<(), SessionError> {
    let mut payload: Map<String, Value> = Map::new();
    payload.insert("event".into(), Value::from("status"));
    payload.insert("state".into(), Value::from(state));
    for (key, value) in extras.iter() {
        if is_droppable(value) {
            continue;
        }
        payload.insert((*key).to_string(), value.clone());
    }
    write_line(writer, &Value::Object(payload))
}

/// Emit one `[worker-event] {…,"event":"utterance",…}` line. Carries
/// the subset of fields `vp_dictate.py::_utterance_event` exposes from
/// the trait surface; the long-tail post-process / format / dictionary
/// fields land with PR 5.
pub(super) fn emit_utterance<W: Write>(
    writer: &mut W,
    text: &str,
    result: &TranscribeResult,
    recording_s: Value,
    inject_error: Option<String>,
) -> Result<(), SessionError> {
    let mut payload: Map<String, Value> = Map::new();
    payload.insert("event".into(), Value::from("utterance"));
    payload.insert("text".into(), Value::from(text));
    payload.insert(
        "text_chars".into(),
        Value::from(text.chars().count() as u64),
    );
    // Clip duration in seconds (rounded to 2 dp). Python's
    // `_utterance_event` writes it; consumers like
    // `src/rust/ui/log_render.rs` + `src/rust/telemetry.rs` read it.
    // Codex P2 #413 wire.rs:61 (round 2).
    payload.insert("recording_s".into(), recording_s);
    payload.insert("compute_ms".into(), Value::from(result.latency_ms));
    // `compute_s` is the seconds-rounded mirror of `compute_ms` that
    // existing consumers (`src/rust/ui/log_render.rs` +
    // `src/rust/telemetry.rs`) still read; the Python emitter writes it
    // alongside the milliseconds field, so we have to too or every
    // Rust-session utterance loses its compute-time in the UI/history.
    // Codex P2 #413.
    payload.insert(
        "compute_s".into(),
        serde_json::json!(round2(result.latency_ms as f64 / 1000.0)),
    );
    payload.insert(
        "audio_duration_s".into(),
        serde_json::json!(round2(result.duration_s)),
    );
    if !result.language.is_empty() {
        payload.insert("language".into(), Value::from(result.language.clone()));
    }
    if let Some(err) = inject_error {
        payload.insert("inject_error".into(), Value::from(err));
    }
    write_line(writer, &Value::Object(payload))
}

/// True for `Value::Null` and the empty-string case, both of which
/// Python's emitter drops via the `if v is not None` filter.
fn is_droppable(value: &Value) -> bool {
    if value.is_null() {
        return true;
    }
    if let Value::String(s) = value {
        return s.is_empty();
    }
    false
}

fn write_line<W: Write>(writer: &mut W, value: &Value) -> Result<(), SessionError> {
    // NOTE: the previous `VOICEPI_WORKER_EVENTS` env-gate that used
    // to live here was moved OUT of this state-machine helper.
    // Rationale: re-reading a process-global env var on EVERY worker
    // event created a Rust 2024 `set_var` race that manifested as a
    // Windows-only flake on `inject_failure_still_emits_utterance`
    // (the assertion "utterance event must still fire on inject
    // failure" panicked mid-cascade). Env-gating is a policy that
    // belongs at the caller boundary -- the RuntimeSupervisor now
    // wraps stderr in a sink writer when `VOICEPI_WORKER_EVENTS`
    // is unset/falsy, so a disabled caller still gets zero output
    // but the state-machine's tests are no longer racing an
    // unsynchronised env-block read. Codex P2 #413 wire.rs:98
    // (round 3 -- follow-up to round 2 which introduced the race).
    writer.write_all(WORKER_EVENT_PREFIX.as_bytes())?;
    // ASCII-escape non-ASCII payload bytes so the worker-event line is
    // safe on Windows shells / hidden subprocess pipes with non-UTF-8
    // code pages. The Python emitter goes through
    // `json.dumps(..., ensure_ascii=True)` which produces the same shape;
    // the existing `test_worker_event_drops_none_fields_and_ascii_encodes`
    // characterisation test pins it. Codex P2 #413.
    //
    // Implementation: serialise to a String first (serde_json's compact
    // form matches Python's `separators=(",", ":")`), then walk
    // codepoint-by-codepoint replacing anything >= U+0080 with a
    // `\uXXXX` BMP escape or a UTF-16 surrogate pair for astral. PR 1's
    // `events::AsciiFormatter` does this inside a `serde_json::Formatter`
    // impl; once PRs 1 + 2 are both in `main`, PR 3 swaps this helper for
    // `events::emit_status` / `events::emit_utterance` directly so the
    // two paths converge.
    let serialised = serde_json::to_string(value).map_err(|e| SessionError::Io(e.to_string()))?;
    write_ascii_escaped(writer, &serialised)?;
    writer.write_all(b"\n")?;
    // Python `_emit_worker_event` uses `flush=True`; PR 1's
    // `events::write_line` flushes too. Without the flush, status lines
    // can sit in a buffered writer past the moment the UI needs them.
    // Codex P2 #413 wire.rs:116 (round 2).
    writer.flush()?;
    Ok(())
}

fn write_ascii_escaped<W: Write>(writer: &mut W, input: &str) -> Result<(), SessionError> {
    let mut buf = String::with_capacity(input.len());
    for ch in input.chars() {
        let cp = ch as u32;
        // Treat DEL (U+007F) as non-ASCII for escaping purposes: Python
        // `json.dumps(ensure_ascii=True)` emits `\u007f` for it, and PR 1's
        // `events::AsciiFormatter` (also in this crate) does the same.
        // Without this branch a dictated string / device label / error
        // message carrying DEL would land as a raw control byte in the
        // worker-event stream and break consumers on shells with
        // non-UTF-8 code pages. Codex P2 #413 wire.rs:146 (round 3).
        if cp < 0x80 && cp != 0x7f {
            buf.push(ch);
            continue;
        }
        if cp < 0x10000 {
            // BMP codepoint: single `\uXXXX` escape (lowercase hex,
            // matching `json.dumps(ensure_ascii=True)`).
            use std::fmt::Write as _;
            write!(&mut buf, "\\u{:04x}", cp).expect("write to String never fails");
            continue;
        }
        // Astral codepoint: UTF-16 surrogate pair, also lowercase hex.
        let cp = cp - 0x10000;
        let high = 0xD800 + (cp >> 10);
        let low = 0xDC00 + (cp & 0x3FF);
        use std::fmt::Write as _;
        write!(&mut buf, "\\u{:04x}\\u{:04x}", high, low).expect("write to String never fails");
    }
    writer.write_all(buf.as_bytes())?;
    Ok(())
}

/// Round to 2 decimal places, matching Python's
/// `round(recording_s, 2)`. Kept local to avoid a dep on a numeric
/// crate.
pub(super) fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
