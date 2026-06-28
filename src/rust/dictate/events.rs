//! Pure-logic worker-event emitter.
//!
//! Mirrors `src/python/whisper_dictate/vp_events.py::_emit_worker_event`
//! byte-for-byte so the Rust runtime supervisor's
//! `runtime::parse_worker_event` consumer can ingest Rust-emitted lines
//! without any wire-format churn.
//!
//! # Wire format (must stay byte-identical to Python)
//!
//! Python emits each event as:
//!
//! ```text
//! [worker-event] {compact-ascii-JSON}\n
//! ```
//!
//! where the JSON payload is produced by
//! `json.dumps(payload, ensure_ascii=True, sort_keys=True,
//! separators=(",", ":"))`. The three knobs all matter:
//!
//! * `ensure_ascii=True` — every non-ASCII codepoint is escaped as
//!   `\uXXXX` (lowercase hex). BMP codepoints emit a single escape;
//!   astral codepoints emit a UTF-16 surrogate pair (`😀`).
//! * `sort_keys=True` — object keys are emitted in alphabetical order.
//! * `separators=(",", ":")` — no whitespace between tokens.
//!
//! `serde_json`'s default `to_writer` uses the compact separators and a
//! `BTreeMap`-backed `serde_json::Map` (sorted), but it does NOT escape
//! non-ASCII; the [`AsciiFormatter`] in this file plugs that gap by
//! wrapping `CompactFormatter::write_string_fragment`.
//!
//! # PR 1 scope
//!
//! Wave 5 PR 1 of #348 adds this module and its tests; production code
//! (the Python orchestrator port) does NOT call it yet. PR 2 wires the
//! Rust supervisor up to emit through this path so the existing
//! `parse_worker_event` consumer keeps working unchanged.
//!
//! No production caller = no behaviour change in this PR; the tests
//! (round-trip through `parse_worker_event`, golden bytes captured from
//! the Python emitter, Unicode escaping, key ordering) lock the
//! byte-equivalence contract before PR 2 starts depending on it.

use std::io::{self, Write};

use serde::Serialize;
use serde_json::ser::{CompactFormatter, Formatter, Serializer};
use serde_json::{Map, Value};

/// Stderr prefix every `[worker-event]` line carries. Matched (with the
/// trailing space) by `runtime::parse_worker_event`.
pub const WORKER_EVENT_PREFIX: &str = "[worker-event] ";

/// The nine canonical worker-status states the Python orchestrator
/// emits across `vp_dictate.py` and `runtime.py`. Wire strings are
/// pinned in [`WorkerStatus::as_wire_str`] so the round-trip through
/// `parse_worker_event` stays exact.
///
/// `post-processing` keeps the hyphen Python uses; renaming it would
/// silently break any UI that switches on the raw state string.
///
/// `Ready` is the default because it is the steady-state the worker
/// settles into between utterances — the natural baseline for a
/// `StatusEvent` built without an explicit state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkerStatus {
    LoadingModel,
    Opening,
    Recording,
    Transcribing,
    PostProcessing,
    NoText,
    Cancelled,
    Error,
    #[default]
    Ready,
}

impl WorkerStatus {
    /// Exact JSON string Python writes for each state.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            WorkerStatus::LoadingModel => "loading_model",
            WorkerStatus::Opening => "opening",
            WorkerStatus::Recording => "recording",
            WorkerStatus::Transcribing => "transcribing",
            WorkerStatus::PostProcessing => "post-processing",
            WorkerStatus::NoText => "no_text",
            WorkerStatus::Cancelled => "cancelled",
            WorkerStatus::Error => "error",
            WorkerStatus::Ready => "ready",
        }
    }
}

/// Structured status event. Mirrors the `status`-event field set the
/// Python orchestrator populates most often (`capture_backend`,
/// `audio_device`, `capture_channels`) and carries an `extras` map for
/// the long tail of per-call fields (e.g. `model`, `gpu`, `reason`,
/// `duration_ms`).
///
/// Optional fields whose value is `None` are dropped before
/// serialisation, matching Python's
/// `payload.update({k: v for k, v in fields.items() if v is not None})`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusEvent {
    pub state: WorkerStatus,
    pub capture_backend: Option<String>,
    pub audio_device: Option<String>,
    pub capture_channels: Option<u32>,
    pub extras: Map<String, Value>,
}

impl StatusEvent {
    /// Build a minimal status event with no optional fields populated.
    pub fn new(state: WorkerStatus) -> Self {
        Self {
            state,
            capture_backend: None,
            audio_device: None,
            capture_channels: None,
            extras: Map::new(),
        }
    }
}

/// Emit a `status` worker-event line through `writer`.
///
/// Writes `[worker-event] <json>\n` where the JSON payload is encoded
/// byte-identically to Python's `_emit_worker_event("status", ...)`.
pub fn emit_status<W: Write>(writer: &mut W, event: &StatusEvent) -> io::Result<()> {
    let mut payload: Map<String, Value> = Map::new();
    payload.insert("event".into(), Value::from("status"));
    payload.insert("state".into(), Value::from(event.state.as_wire_str()));
    if let Some(value) = event.capture_backend.as_ref() {
        payload.insert("capture_backend".into(), Value::from(value.clone()));
    }
    if let Some(value) = event.audio_device.as_ref() {
        payload.insert("audio_device".into(), Value::from(value.clone()));
    }
    if let Some(value) = event.capture_channels {
        payload.insert("capture_channels".into(), Value::from(value));
    }
    // Extras win when keys collide — matches Python's `payload.update(fields)`
    // semantics (later assignments overwrite earlier ones).
    for (key, value) in event.extras.iter() {
        if value.is_null() {
            continue;
        }
        payload.insert(key.clone(), value.clone());
    }
    write_line(writer, &Value::Object(payload))
}

/// Emit an `utterance` worker-event line through `writer`.
///
/// `payload` must be a JSON object; the `"event": "utterance"` key is
/// inserted before serialisation. Null-valued keys in `payload` are
/// dropped, matching Python's `if v is not None` filter.
pub fn emit_utterance<W: Write>(writer: &mut W, payload: &Value) -> io::Result<()> {
    write_named_event(writer, "utterance", payload)
}

/// Emit an `error` worker-event line through `writer`.
///
/// `message` is inserted under the `"message"` key; `payload` carries
/// any additional fields (typically `state="failed"`, `backend`,
/// `model`). The `"event": "error"` key is inserted before
/// serialisation. Null-valued keys are dropped.
pub fn emit_error<W: Write>(writer: &mut W, message: &str, payload: &Value) -> io::Result<()> {
    let mut merged: Map<String, Value> = Map::new();
    if let Some(object) = payload.as_object() {
        for (key, value) in object.iter() {
            if value.is_null() || key == "event" || key == "message" {
                continue;
            }
            merged.insert(key.clone(), value.clone());
        }
    }
    merged.insert("message".into(), Value::from(message));
    write_named_event(writer, "error", &Value::Object(merged))
}

fn write_named_event<W: Write>(writer: &mut W, name: &str, payload: &Value) -> io::Result<()> {
    let mut out: Map<String, Value> = Map::new();
    out.insert("event".into(), Value::from(name));
    if let Some(object) = payload.as_object() {
        for (key, value) in object.iter() {
            // Drop None-equivalents and protect the canonical "event" key.
            if value.is_null() || key == "event" {
                continue;
            }
            out.insert(key.clone(), value.clone());
        }
    }
    write_line(writer, &Value::Object(out))
}

/// Encode `value` with the Python-equivalent JSON dialect and write the
/// `[worker-event] <json>\n` line.
fn write_line<W: Write>(writer: &mut W, value: &Value) -> io::Result<()> {
    writer.write_all(WORKER_EVENT_PREFIX.as_bytes())?;
    let mut serializer = Serializer::with_formatter(&mut *writer, AsciiFormatter::new());
    value.serialize(&mut serializer).map_err(io::Error::other)?;
    writer.write_all(b"\n")
}

/// A `serde_json` formatter that produces Python `ensure_ascii=True`
/// output: every non-ASCII codepoint becomes a `\uXXXX` escape
/// (lowercase hex), and astral codepoints emit a UTF-16 surrogate pair.
///
/// Inherits compact separators (`,` / `:`) from
/// [`CompactFormatter`]; `serde_json::Map` is `BTreeMap`-backed (no
/// `preserve_order` feature on `serde_json` in this crate), so key
/// order is alphabetical — matching Python's `sort_keys=True`.
struct AsciiFormatter {
    inner: CompactFormatter,
}

impl AsciiFormatter {
    fn new() -> Self {
        Self {
            inner: CompactFormatter,
        }
    }
}

impl Formatter for AsciiFormatter {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        let mut start = 0;
        let bytes = fragment.as_bytes();
        for (idx, ch) in fragment.char_indices() {
            if (ch as u32) < 0x80 {
                continue;
            }
            // Flush the ASCII run up to this codepoint.
            if start < idx {
                writer.write_all(&bytes[start..idx])?;
            }
            write_unicode_escape(writer, ch)?;
            start = idx + ch.len_utf8();
        }
        if start < bytes.len() {
            writer.write_all(&bytes[start..])?;
        }
        Ok(())
    }

    // Delegate every other Formatter hook to CompactFormatter so the
    // structural punctuation (`,` `:` `[` `]` `{` `}` `"`) and number
    // formatting stay bit-identical to serde_json's compact output.
    fn write_null<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.write_null(writer)
    }
    fn write_bool<W: ?Sized + Write>(&mut self, writer: &mut W, value: bool) -> io::Result<()> {
        self.inner.write_bool(writer, value)
    }
    fn write_i8<W: ?Sized + Write>(&mut self, writer: &mut W, value: i8) -> io::Result<()> {
        self.inner.write_i8(writer, value)
    }
    fn write_i16<W: ?Sized + Write>(&mut self, writer: &mut W, value: i16) -> io::Result<()> {
        self.inner.write_i16(writer, value)
    }
    fn write_i32<W: ?Sized + Write>(&mut self, writer: &mut W, value: i32) -> io::Result<()> {
        self.inner.write_i32(writer, value)
    }
    fn write_i64<W: ?Sized + Write>(&mut self, writer: &mut W, value: i64) -> io::Result<()> {
        self.inner.write_i64(writer, value)
    }
    fn write_i128<W: ?Sized + Write>(&mut self, writer: &mut W, value: i128) -> io::Result<()> {
        self.inner.write_i128(writer, value)
    }
    fn write_u8<W: ?Sized + Write>(&mut self, writer: &mut W, value: u8) -> io::Result<()> {
        self.inner.write_u8(writer, value)
    }
    fn write_u16<W: ?Sized + Write>(&mut self, writer: &mut W, value: u16) -> io::Result<()> {
        self.inner.write_u16(writer, value)
    }
    fn write_u32<W: ?Sized + Write>(&mut self, writer: &mut W, value: u32) -> io::Result<()> {
        self.inner.write_u32(writer, value)
    }
    fn write_u64<W: ?Sized + Write>(&mut self, writer: &mut W, value: u64) -> io::Result<()> {
        self.inner.write_u64(writer, value)
    }
    fn write_u128<W: ?Sized + Write>(&mut self, writer: &mut W, value: u128) -> io::Result<()> {
        self.inner.write_u128(writer, value)
    }
    fn write_f32<W: ?Sized + Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        self.inner.write_f32(writer, value)
    }
    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        self.inner.write_f64(writer, value)
    }
    fn write_number_str<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        value: &str,
    ) -> io::Result<()> {
        self.inner.write_number_str(writer, value)
    }
    fn begin_string<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.begin_string(writer)
    }
    fn end_string<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.end_string(writer)
    }
    fn write_char_escape<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        char_escape: serde_json::ser::CharEscape,
    ) -> io::Result<()> {
        self.inner.write_char_escape(writer, char_escape)
    }
    fn begin_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.begin_array(writer)
    }
    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.end_array(writer)
    }
    fn begin_array_value<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.inner.begin_array_value(writer, first)
    }
    fn end_array_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.end_array_value(writer)
    }
    fn begin_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.begin_object(writer)
    }
    fn end_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.end_object(writer)
    }
    fn begin_object_key<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.inner.begin_object_key(writer, first)
    }
    fn end_object_key<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.end_object_key(writer)
    }
    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.begin_object_value(writer)
    }
    fn end_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.end_object_value(writer)
    }
    fn write_raw_fragment<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        self.inner.write_raw_fragment(writer, fragment)
    }
}

/// Re-serialize `ch` as Python's `ensure_ascii=True` would: lowercase
/// `\uXXXX` for BMP codepoints, a UTF-16 surrogate pair for astral
/// codepoints.
fn write_unicode_escape<W: ?Sized + Write>(writer: &mut W, ch: char) -> io::Result<()> {
    let cp = ch as u32;
    if cp <= 0xFFFF {
        write!(writer, "\\u{:04x}", cp)
    } else {
        let adjusted = cp - 0x10000;
        let high = 0xD800 + (adjusted >> 10);
        let low = 0xDC00 + (adjusted & 0x3FF);
        write!(writer, "\\u{:04x}\\u{:04x}", high, low)
    }
}
