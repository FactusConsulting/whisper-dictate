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
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::types::{SessionConfig, SessionError, TranscribeResult};
use crate::platform::foreground_window::WindowInfo;

/// Compact-text helper for `text_preview`, mirroring Python's
/// `_compact_text(text, limit=240)` in `vp_events.py`: collapse
/// whitespace runs to single spaces, then truncate at `limit` (append
/// `...` when clipped). Kept local so wire.rs owns the whole utterance
/// payload shape. Codex P1 #606 metrics-schema follow-up.
pub(super) const TEXT_PREVIEW_LIMIT: usize = 240;

pub(super) fn compact_text(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TEXT_PREVIEW_LIMIT {
        return collapsed;
    }
    // Truncate on a char boundary and add "..." (3 chars) so the total
    // never exceeds `TEXT_PREVIEW_LIMIT` visible chars -- matching
    // Python's `text[: limit - 3] + "..."`.
    let cut = collapsed
        .char_indices()
        .nth(TEXT_PREVIEW_LIMIT.saturating_sub(3))
        .map(|(i, _)| i)
        .unwrap_or(collapsed.len());
    let mut out = collapsed[..cut].to_owned();
    out.push_str("...");
    out
}

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

/// Extra context bundled with an `emit_utterance` call so the payload
/// carries every field `vp_dictate.py::_utterance_event` populates.
/// Kept as a small parameter struct so the callsite is readable.
/// Codex P1 #606 metrics-schema follow-up.
#[derive(Debug, Clone)]
pub(super) struct UtteranceExtras<'a> {
    /// Post-dictionary, pre-postprocess text (Python's `source_text`
    /// -> `dictionary_text`). Falls back to the final injected text
    /// when the dictionary made no rewrite.
    pub dictionary_text: &'a str,
    /// Foreground-window info captured at [`super::DictateSession::start`]
    /// via the profile matcher's probe. Emits `target_title` /
    /// `target_process` when non-empty (Python's `_inject_target_title`
    /// / `_inject_target_process`).
    pub window: Option<&'a WindowInfo>,
    /// Name of the profile that fired for this utterance (Python's
    /// `_active_profile_name`). Emitted as `profile` when present;
    /// empty string is dropped to match Python's `None` filter.
    pub profile: Option<&'a str>,
    /// Session-level backend metadata (`stt_backend`, `model`, `device`,
    /// `compute_type`, `inject_mode`). Empty strings on any field
    /// are dropped so a bare-Default session (unit tests) still emits
    /// a clean payload.
    pub config: &'a SessionConfig,
}

/// Emit one `[worker-event] {…,"event":"utterance",…}` line and return the
/// serialised payload so the session can also hand it to the history sink
/// (parity with Python's `vp_dictate._record_utterance_event`, which calls
/// `_emit_worker_event` and `append_record_sinks` from the same event dict).
///
/// Carries the full field set `vp_dictate.py::_utterance_event` exposes
/// from the trait surface, plus the `post_*` post-processing metadata when
/// a pass ran (`post` is `Some`), matching `vp_dictate.py:469-475`.
pub(super) fn emit_utterance<W: Write>(
    writer: &mut W,
    text: &str,
    result: &TranscribeResult,
    recording_s: Value,
    inject_error: Option<String>,
    post: Option<&super::PostProcessOutcome>,
    replacements: &[crate::dictionary::ReplacementChange],
    extras: UtteranceExtras<'_>,
) -> Result<Value, SessionError> {
    let payload = build_utterance_payload(
        text,
        result,
        recording_s,
        inject_error,
        post,
        replacements,
        extras,
    );
    write_line(writer, &payload)?;
    Ok(payload)
}

/// Assemble the utterance payload without emitting it. Split out so the
/// session can build the payload once and share it with both the wire-format
/// emitter and the history sink (Python parity: `_utterance_event` is built
/// once, then `_emit_worker_event` and `append_record_sinks` both consume it).
pub(super) fn build_utterance_payload(
    text: &str,
    result: &TranscribeResult,
    recording_s: Value,
    inject_error: Option<String>,
    post: Option<&super::PostProcessOutcome>,
    replacements: &[crate::dictionary::ReplacementChange],
    extras: UtteranceExtras<'_>,
) -> Value {
    let mut payload: Map<String, Value> = Map::new();
    // Python's `_base_event` stamps `ts = time.time()` (float seconds since
    // the Unix epoch) on every utterance dict before `_emit_worker_event` +
    // `append_record_sinks` see it. Without this, history rows written from
    // the Rust engine lack a timestamp and the `history list` renderer shows
    // blank leading timestamps. `SystemTime::now()` failure (clock before
    // epoch) is exceedingly rare on a running machine; fall back to 0.0 so a
    // clock-glitched session still records the utterance rather than
    // panicking.
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    payload.insert("ts".into(), serde_json::json!(ts));
    payload.insert("event".into(), Value::from("utterance"));
    payload.insert("text".into(), Value::from(text));
    payload.insert(
        "text_chars".into(),
        Value::from(text.chars().count() as u64),
    );
    // `text_preview` (Python's `_compact_text(text)`): whitespace-collapsed
    // + 240-char truncated form. Emitted verbatim on the utterance event so
    // metrics/history rows carry a display-safe short form. Codex P1 #606.
    payload.insert("text_preview".into(), Value::from(compact_text(text)));
    // `raw_text` (Python's `result.raw_text or source_text`): the
    // backend's untouched decoded text; falls back to the post-dictionary
    // form so metrics never carry an empty string when the backend does
    // not surface a separate raw copy. Codex P1 #606.
    let raw_text = if result.raw_text.is_empty() {
        if extras.dictionary_text.is_empty() {
            text.to_owned()
        } else {
            extras.dictionary_text.to_owned()
        }
    } else {
        result.raw_text.clone()
    };
    payload.insert("raw_text".into(), Value::from(raw_text));
    // `dictionary_text` (Python's `source_text`): text AFTER the
    // dictionary rewrite pass but BEFORE post-processing / format
    // commands. Falls back to the final text when the dictionary was a
    // passthrough. Codex P1 #606.
    let dictionary_text = if extras.dictionary_text.is_empty() {
        text
    } else {
        extras.dictionary_text
    };
    payload.insert(
        "dictionary_text".into(),
        Value::from(dictionary_text.to_owned()),
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
    // `real_time_factor` (Python: `result.real_time_factor`) is the
    // compute-to-audio ratio -- speed of transcription relative to how
    // long the audio was. Emitted as 0.0 when the audio duration is
    // zero (silent buffer, empty capture) so downstream tooling never
    // divides by zero. Codex P1 #606.
    let rtf = if result.duration_s > 0.0 {
        round2((result.latency_ms as f64 / 1000.0) / result.duration_s)
    } else {
        0.0
    };
    payload.insert("real_time_factor".into(), serde_json::json!(rtf));
    if !result.language.is_empty() {
        payload.insert("language".into(), Value::from(result.language.clone()));
    }
    if result.language_probability > 0.0 {
        // `language_probability` (Python: `result.language_probability`).
        // Only emitted when the backend surfaced a score, matching
        // Python's `None`-drop for optional numeric fields.
        payload.insert(
            "language_probability".into(),
            serde_json::json!(round2(result.language_probability)),
        );
    }
    // Session-level backend metadata (Python's `_transcription_event_fields`
    // + `_inject_event_fields`). Each field is dropped when empty so a
    // bare-Default `SessionConfig` (unit tests) still yields a clean
    // payload; production wiring in `rust_session_real_backends.rs`
    // populates the values from the process env / resolved backend.
    insert_non_empty(&mut payload, "stt_backend", &extras.config.stt_backend);
    insert_non_empty(&mut payload, "model", &extras.config.model);
    insert_non_empty(&mut payload, "device", &extras.config.device);
    insert_non_empty(&mut payload, "compute_type", &extras.config.compute_type);
    insert_non_empty(&mut payload, "inject_mode", &extras.config.inject_mode);
    // Target-window info + profile name (Python's `_inject_target_title`,
    // `_inject_target_process`, `_active_profile_name`). Each field is
    // dropped when empty so a session without a profile matcher (unit
    // tests, `simulate-session`) emits nothing here.
    if let Some(window) = extras.window {
        if let Some(title) = window.title.as_deref() {
            insert_non_empty(&mut payload, "target_title", title);
        }
        if let Some(process) = window.process.as_deref() {
            insert_non_empty(&mut payload, "target_process", process);
        }
    }
    if let Some(profile) = extras.profile {
        insert_non_empty(&mut payload, "profile", profile);
    }
    if let Some(err) = inject_error {
        payload.insert("inject_error".into(), Value::from(err));
    }
    // Post-processing metadata (only when a pass ran), mirroring the
    // `post_*` fields `vp_dictate.py:469-475` emits so
    // `ui/log_render.rs::post_processing_summary` shows the active
    // provider/mode and telemetry/history record latency + failures.
    if let Some(p) = post {
        payload.insert("post_processor".into(), Value::from(p.processor.clone()));
        payload.insert("post_mode".into(), Value::from(p.mode.clone()));
        payload.insert("post_model".into(), Value::from(p.model.clone()));
        payload.insert("post_latency_ms".into(), Value::from(p.latency_ms));
        payload.insert("post_changed".into(), Value::from(p.changed));
        payload.insert("post_fallback".into(), Value::from(p.fallback));
        // Python emits `error or None`; drop the field when empty so the
        // UI does not render a blank error.
        if !p.error.is_empty() {
            payload.insert("post_error".into(), Value::from(p.error.clone()));
        }
        // Redaction provenance (public-safe: placeholder / kind / char
        // count only). Always emitted when a pass ran -- `post_redactions`
        // is `[]` when nothing was redacted, matching Python's
        // `redactions or []`.
        payload.insert("post_redacted".into(), Value::from(p.redacted));
        let redactions: Vec<Value> = p
            .redactions
            .iter()
            .map(|r| {
                serde_json::json!({
                    "placeholder": r.placeholder,
                    "kind": r.kind,
                    "chars": r.chars,
                })
            })
            .collect();
        payload.insert("post_redactions".into(), Value::from(redactions));
    }
    // Dictionary replacements that fired (Python's `dictionary_replacements`),
    // as an array of {from, to, count}. Emitted only when non-empty so a
    // no-replacement utterance stays clean; `ui/log_render.rs` counts the array
    // to show `replacements=N` and telemetry/history keep the records.
    if !replacements.is_empty() {
        let entries: Vec<Value> = replacements
            .iter()
            .map(|c| {
                serde_json::json!({
                    "from": c.from,
                    "to": c.to,
                    "count": c.count,
                })
            })
            .collect();
        payload.insert("dictionary_replacements".into(), Value::from(entries));
    }
    Value::Object(payload)
}

/// Insert `value` into `payload` under `key` iff the string is non-empty
/// after trimming. Matches Python's per-field drop-on-empty behaviour
/// (`_emit_worker_event`'s `if value is not None` + `_utterance_event`'s
/// omission of blank-string fields). Codex P1 #606.
fn insert_non_empty(payload: &mut Map<String, Value>, key: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    payload.insert(key.to_owned(), Value::from(value.to_owned()));
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
    // Honour the VOICEPI_WORKER_EVENTS env gate (Python's
    // `_emit_worker_event` returns early when the var is unset/falsy;
    // PR 1's `events::write_line` does the same). Without the gate any
    // session run outside the RuntimeSupervisor (e.g. a CLI smoke or
    // tooling integration) would leak lines to whatever writer was
    // passed in. Codex P2 #413 wire.rs:98 (round 2).
    if !crate::dictate::env_gates::is_truthy(std::env::var("VOICEPI_WORKER_EVENTS").ok().as_deref())
    {
        return Ok(());
    }
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
