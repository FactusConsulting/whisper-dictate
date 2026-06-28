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
//! * `ensure_ascii=True` — every non-ASCII codepoint (and DEL, U+007F)
//!   is escaped as `\uXXXX` (lowercase hex). BMP codepoints emit a
//!   single escape; astral codepoints emit a UTF-16 surrogate pair.
//! * `sort_keys=True` — object keys are emitted in alphabetical order.
//! * `separators=(",", ":")` — no whitespace between tokens.
//!
//! `serde_json`'s default `to_writer` uses the compact separators and a
//! `BTreeMap`-backed `serde_json::Map` (sorted), but it does NOT escape
//! non-ASCII and its float formatter uses minimum-digit exponents
//! (`1e-6` rather than CPython's `1e-06`); the [`AsciiFormatter`] in
//! this file plugs both gaps.
//!
//! # Env-gate: `VOICEPI_WORKER_EVENTS`
//!
//! Python's `_emit_worker_event` short-circuits to a no-op unless
//! `VOICEPI_WORKER_EVENTS` is truthy (`_truthy` from `vp_events.py`).
//! [`write_line`] enforces the same gate so opting out at the env layer
//! turns every Rust emitter into a no-op without any caller changes.
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

/// Env var that gates emission. Matches
/// `vp_events.py::_emit_worker_event`'s `VOICEPI_WORKER_EVENTS` check
/// (truthy by `runtime._truthy` rules).
pub(crate) const WORKER_EVENTS_ENV: &str = "VOICEPI_WORKER_EVENTS";

/// The eleven canonical worker-status states the Python orchestrator
/// emits across `vp_dictate.py`, `vp_capture.py`, `vp_preview.py`, and
/// `runtime.py`. Wire strings are pinned in [`WorkerStatus::as_wire_str`]
/// so the round-trip through `parse_worker_event` (and the Rust UI's
/// state-switch ladder in `src/rust/ui/app.rs`) stays exact.
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
    /// Mid-recording display-only signal emitted from
    /// `vp_preview.py` while a live transcription preview is updating.
    /// The Rust UI special-cases this state (does NOT clear the
    /// "recording" pipeline stage), so a typo in the wire string would
    /// silently break the live preview path.
    Preview,
    /// Emitted from `vp_capture.py` when the capture reader hits an
    /// unrecoverable error mid-recording (device unplugged, etc.).
    CaptureLost,
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
            WorkerStatus::Preview => "preview",
            WorkerStatus::CaptureLost => "capture_lost",
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

/// Live audio-meter event. Mirrors the field set
/// `vp_capture.py::_emit_audio_level` populates: `level`, `raw_dbfs`,
/// `peak` (all already rounded by the caller to match Python's
/// `round(..., n)` truncation), plus the capture backend/device/channel
/// triple that's also threaded through `status` events. State is
/// hard-coded to `"recording"` because that is the only value Python
/// ever passes from this code path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioEvent {
    pub level: f64,
    pub raw_dbfs: f64,
    pub peak: f64,
    pub capture_backend: Option<String>,
    pub audio_device: Option<String>,
    pub capture_channels: Option<u32>,
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
    // semantics — EXCEPT for the canonical `"event"` key, which must
    // always be the literal `"status"` so `parse_worker_event` can
    // dispatch. Mirrors the guard `write_named_event` applies for
    // `emit_utterance` / `emit_error`.
    for (key, value) in event.extras.iter() {
        if value.is_null() || key == "event" {
            continue;
        }
        payload.insert(key.clone(), value.clone());
    }
    write_line(writer, &Value::Object(payload))
}

/// Emit an `audio` worker-event line through `writer`.
///
/// Mirrors `vp_capture.py::_emit_audio_level` byte-for-byte: state is
/// always `"recording"`, the three metrics are always present, and the
/// capture backend/device/channel fields are dropped when `None`.
pub fn emit_audio<W: Write>(writer: &mut W, event: &AudioEvent) -> io::Result<()> {
    let mut payload: Map<String, Value> = Map::new();
    payload.insert("event".into(), Value::from("audio"));
    payload.insert("state".into(), Value::from("recording"));
    payload.insert("level".into(), Value::from(event.level));
    payload.insert("raw_dbfs".into(), Value::from(event.raw_dbfs));
    payload.insert("peak".into(), Value::from(event.peak));
    if let Some(value) = event.capture_backend.as_ref() {
        payload.insert("capture_backend".into(), Value::from(value.clone()));
    }
    if let Some(value) = event.audio_device.as_ref() {
        payload.insert("audio_device".into(), Value::from(value.clone()));
    }
    if let Some(value) = event.capture_channels {
        payload.insert("capture_channels".into(), Value::from(value));
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
/// `[worker-event] <json>\n` line. Flushes after the newline so a
/// downstream `parse_worker_event` consumer reading line-by-line sees
/// the event the moment it is emitted — Python's
/// `print(..., flush=True)` does the same.
///
/// Short-circuits to a no-op unless `VOICEPI_WORKER_EVENTS` is truthy
/// per [`crate::dictate::env_gates::is_truthy`], matching the env-gate
/// in `vp_events.py::_emit_worker_event`.
fn write_line<W: Write>(writer: &mut W, value: &Value) -> io::Result<()> {
    if !worker_events_enabled() {
        return Ok(());
    }
    writer.write_all(WORKER_EVENT_PREFIX.as_bytes())?;
    let mut serializer = Serializer::with_formatter(&mut *writer, AsciiFormatter::new());
    value.serialize(&mut serializer).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// True when the `VOICEPI_WORKER_EVENTS` env var is truthy by Python's
/// `runtime._truthy` rules (anything but `""`, `"0"`, `"false"`,
/// `"no"`, `"off"` after `.strip().lower()`).
fn worker_events_enabled() -> bool {
    crate::dictate::env_gates::is_truthy(std::env::var(WORKER_EVENTS_ENV).ok().as_deref())
}

/// A `serde_json` formatter that produces Python `ensure_ascii=True`
/// output: every non-ASCII codepoint (and the DEL control, U+007F)
/// becomes a `\uXXXX` escape (lowercase hex), and astral codepoints
/// emit a UTF-16 surrogate pair. Float output is also re-formatted to
/// match CPython's `repr(float)` — see [`write_python_float`].
///
/// Inherits compact separators (`,` / `:`) from [`CompactFormatter`];
/// `serde_json::Map` is `BTreeMap`-backed (no `preserve_order` feature
/// on `serde_json` in this crate), so key order is alphabetical —
/// matching Python's `sort_keys=True`.
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
            // Python's `ensure_ascii=True` escapes everything outside the
            // printable ASCII range 0x20..=0x7E — including DEL (0x7F),
            // which `json.dumps('\x7f', ensure_ascii=True)` writes as
            // `""`. CompactFormatter's `write_string_fragment`
            // would let DEL through verbatim, so we treat 0x7F like the
            // non-ASCII branch below.
            let cp = ch as u32;
            if cp < 0x80 && cp != 0x7F {
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

    // Floats are reformatted to match CPython's `repr(float)` (see
    // `write_python_float`): scientific-notation exponents are padded
    // to 2+ digits with an explicit sign (`1e-06`, `1e+16`) and the
    // fixed/scientific switchover happens at |x| < 1e-4 / |x| >= 1e16
    // — both differ from serde_json's default formatter.
    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        write_python_float(writer, value)
    }
    fn write_f32<W: ?Sized + Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        write_python_float(writer, value as f64)
    }

    // Delegate the rest to CompactFormatter so the structural
    // punctuation (`,` `:` `[` `]` `{` `}` `"`) and integer
    // formatting stay bit-identical to serde_json's compact output.
    // Only the methods that `serde_json::Value` actually reaches
    // (i32/i64/u32/u64, bool/null, the array/object/string scaffolding)
    // are listed; the unused integer widths (i8/i16/u8/u16 and
    // i128/u128) fall through to the trait's default impls, which also
    // go through `itoa` and produce identical bytes.
    fn write_null<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.write_null(writer)
    }
    fn write_bool<W: ?Sized + Write>(&mut self, writer: &mut W, value: bool) -> io::Result<()> {
        self.inner.write_bool(writer, value)
    }
    fn write_i32<W: ?Sized + Write>(&mut self, writer: &mut W, value: i32) -> io::Result<()> {
        self.inner.write_i32(writer, value)
    }
    fn write_i64<W: ?Sized + Write>(&mut self, writer: &mut W, value: i64) -> io::Result<()> {
        self.inner.write_i64(writer, value)
    }
    fn write_u32<W: ?Sized + Write>(&mut self, writer: &mut W, value: u32) -> io::Result<()> {
        self.inner.write_u32(writer, value)
    }
    fn write_u64<W: ?Sized + Write>(&mut self, writer: &mut W, value: u64) -> io::Result<()> {
        self.inner.write_u64(writer, value)
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

/// Format `value` to match CPython's `repr(float)` (and therefore
/// `json.dumps`) byte-for-byte.
///
/// CPython produces the shortest round-trip decimal representation and
/// then picks fixed vs scientific notation by the boundary
/// |value| < 1e-4 → scientific, |value| >= 1e16 → scientific, else
/// fixed. The scientific form always pads the exponent to at least two
/// digits with an explicit sign (`1e-06`, `1e+16`) and strips any
/// trailing `.0` from the mantissa (`1e-06` not `1.0e-06`).
///
/// serde_json's `CompactFormatter::write_f64` also emits shortest
/// round-trip digits but uses a different boundary (its fixed-point
/// range extends down to ~1e-5) and a minimum-digit exponent (`1e-6`).
/// We therefore take its output and reformat the parts that differ:
///
/// 1. If it's already scientific, just rewrite the exponent.
/// 2. If it's fixed but |value| < 1e-4, convert to scientific
///    by counting the leading zeros after `0.`.
/// 3. Otherwise (fixed and Python agrees), pass through unchanged.
///
/// `-0.0` / `+0.0` short-circuit to the literal Python output. NaN /
/// +/-Infinity hand back to the inner formatter — in practice
/// `serde_json::Number::from_f64` rejects them so the branch is
/// unreachable from `Value`, but keeping parity with serde_json's
/// default keeps any direct serializer caller from seeing surprise
/// behaviour from this formatter alone.
fn write_python_float<W: ?Sized + Write>(writer: &mut W, value: f64) -> io::Result<()> {
    if !value.is_finite() {
        return CompactFormatter.write_f64(writer, value);
    }
    if value == 0.0 {
        return writer.write_all(if value.is_sign_negative() {
            b"-0.0"
        } else {
            b"0.0"
        });
    }

    // Start from serde_json's shortest round-trip output. We only need
    // to reformat the cosmetic parts (boundary + exponent padding).
    let mut buf: Vec<u8> = Vec::with_capacity(32);
    CompactFormatter.write_f64(&mut buf, value)?;
    let s = std::str::from_utf8(&buf).expect("CompactFormatter writes ASCII");

    let abs = value.abs();
    if let Some(e_idx) = s.find(['e', 'E']) {
        // Already scientific — just rewrite the exponent (and strip a
        // trailing `.0` from the mantissa to match Python's `1e-06`,
        // not `1.0e-06`).
        let mantissa = s[..e_idx].strip_suffix(".0").unwrap_or(&s[..e_idx]);
        let exp: i32 = s[e_idx + 1..].parse().map_err(io::Error::other)?;
        write!(writer, "{}e{:+03}", mantissa, exp)
    } else if abs < 1e-4 {
        // Fixed output but Python would use scientific. serde_json's
        // output in this range is always of the form `[-]0.0...digits`.
        let (sign, rest) = match s.strip_prefix('-') {
            Some(r) => ("-", r),
            None => ("", s),
        };
        let after_dot = rest
            .strip_prefix("0.")
            .expect("|value| < 1e-4 keeps a leading `0.` in fixed-point");
        let leading_zeros = after_dot.bytes().take_while(|&b| b == b'0').count();
        let digits = &after_dot[leading_zeros..];
        let exp = -(leading_zeros as i32 + 1);
        if digits.len() == 1 {
            write!(writer, "{}{}e{:+03}", sign, digits, exp)
        } else {
            write!(
                writer,
                "{}{}.{}e{:+03}",
                sign,
                &digits[..1],
                &digits[1..],
                exp
            )
        }
    } else {
        // Both formats agree (fixed-point and |value| in [1e-4, 1e16)).
        writer.write_all(s.as_bytes())
    }
}
