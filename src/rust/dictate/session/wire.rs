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
    inject_error: Option<String>,
) -> Result<(), SessionError> {
    let mut payload: Map<String, Value> = Map::new();
    payload.insert("event".into(), Value::from("utterance"));
    payload.insert("text".into(), Value::from(text));
    payload.insert(
        "text_chars".into(),
        Value::from(text.chars().count() as u64),
    );
    payload.insert("compute_ms".into(), Value::from(result.latency_ms));
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
    writer.write_all(WORKER_EVENT_PREFIX.as_bytes())?;
    serde_json::to_writer(&mut *writer, value).map_err(|e| SessionError::Io(e.to_string()))?;
    writer.write_all(b"\n")?;
    Ok(())
}

/// Round to 2 decimal places, matching Python's
/// `round(recording_s, 2)`. Kept local to avoid a dep on a numeric
/// crate.
pub(super) fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
