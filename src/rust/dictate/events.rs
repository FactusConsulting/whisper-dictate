//! Pure-logic worker-event emitter.
//!
//! Emits the worker-event lines the Rust runtime supervisor's
//! `runtime::parse_worker_event` consumer ingests.
//!
//! # Wire format
//!
//! Each event is emitted as:
//!
//! ```text
//! [worker-event] {compact-ascii-JSON}\n
//! ```
//!
//! The three knobs that shape the JSON payload all matter:
//!
//! * ASCII-only — every non-ASCII codepoint (and DEL, U+007F) is
//!   escaped as `\uXXXX` (lowercase hex). BMP codepoints emit a single
//!   escape; astral codepoints emit a UTF-16 surrogate pair.
//! * Sorted keys — object keys are emitted in alphabetical order.
//! * Compact separators — no whitespace between tokens.
//!
//! `serde_json`'s default `to_writer` uses the compact separators and a
//! `BTreeMap`-backed `serde_json::Map` (sorted), but it does NOT escape
//! non-ASCII and its float formatter uses minimum-digit exponents
//! (`1e-6` rather than `1e-06`); the [`AsciiFormatter`] in this file
//! plugs both gaps.
//!
//! # Env-gate: `VOICEPI_WORKER_EVENTS`
//!
//! The emitter short-circuits to a no-op unless `VOICEPI_WORKER_EVENTS`
//! is truthy. [`write_line`] enforces the gate so opting out at the env
//! layer turns every emitter into a no-op without any caller changes.
//!
//! The tests (round-trip through `parse_worker_event`, golden bytes,
//! Unicode escaping, key ordering) lock the byte-level contract the
//! runtime supervisor depends on.

use std::io::{self, Write};

use serde::Serialize;
use serde_json::ser::{CompactFormatter, Formatter, Serializer};
use serde_json::{Map, Value};

/// Stderr prefix every `[worker-event]` line carries. Matched (with the
/// trailing space) by `runtime::parse_worker_event`.
pub const WORKER_EVENT_PREFIX: &str = "[worker-event] ";

/// Env var that gates emission (truthy by the shared env-truthy rules).
pub(crate) const WORKER_EVENTS_ENV: &str = "VOICEPI_WORKER_EVENTS";

/// Selects who owns the decision to emit worker events.
///
/// Process-boundary callers retain the historical environment gate, while
/// the in-process runtime opts in explicitly on its owned [`DictateSession`]
/// and never mutates the process environment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum WorkerEventOutput {
    #[default]
    Environment,
    Enabled,
}

impl WorkerEventOutput {
    pub(crate) fn is_enabled(self) -> bool {
        match self {
            Self::Environment => worker_events_enabled(),
            Self::Enabled => true,
        }
    }
}

/// The canonical worker-status states emitted by the runtime. Wire
/// strings are pinned in [`WorkerStatus::as_wire_str`]
/// so the round-trip through `parse_worker_event` (and the Rust UI's
/// state-switch ladder in `src/rust/ui/app.rs`) stays exact.
///
/// `post-processing` keeps its hyphen; renaming it would
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
    Injecting,
    NoText,
    Cancelled,
    Error,
    /// Mid-recording display-only signal emitted while a live
    /// transcription preview is updating.
    /// The Rust UI special-cases this state (does NOT clear the
    /// "recording" pipeline stage), so a typo in the wire string would
    /// silently break the live preview path.
    Preview,
    /// Emitted when the capture reader hits an
    /// unrecoverable error mid-recording (device unplugged, etc.).
    CaptureLost,
    /// The configured microphone was unavailable and the system default
    /// completed the runtime's bounded health validation.
    AudioFallback,
    /// A microphone opened during background recovery and completed the
    /// runtime's bounded health validation.
    AudioRecovered,
    #[default]
    Ready,
}

impl WorkerStatus {
    /// Exact JSON string for each state.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            WorkerStatus::LoadingModel => "loading_model",
            WorkerStatus::Opening => "opening",
            WorkerStatus::Recording => "recording",
            WorkerStatus::Transcribing => "transcribing",
            WorkerStatus::PostProcessing => "post-processing",
            WorkerStatus::Injecting => "injecting",
            WorkerStatus::NoText => "no_text",
            WorkerStatus::Cancelled => "cancelled",
            WorkerStatus::Error => "error",
            WorkerStatus::Preview => "preview",
            WorkerStatus::CaptureLost => "capture_lost",
            WorkerStatus::AudioFallback => "audio-fallback",
            WorkerStatus::AudioRecovered => "audio-recovered",
            WorkerStatus::Ready => "ready",
        }
    }
}

/// Structured status event. Common fields (`capture_backend`,
/// `audio_device`, `capture_channels`) sit at the top level and an
/// `extras` map carries the long tail of per-call fields (e.g.
/// `model`, `gpu`, `reason`, `duration_ms`).
///
/// Optional fields whose value is `None` are dropped before
/// serialisation.
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

/// Live audio-meter event. Fields: `level`, `raw_dbfs`, `peak` (all
/// already rounded by the caller to a fixed digit count), plus the
/// capture backend/device/channel triple that's also threaded through
/// `status` events. State is hard-coded to `"recording"` because that
/// is the only value this code path passes.
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
/// Writes `[worker-event] <json>\n` for a status event.
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
    // Extras win when keys collide — EXCEPT for the canonical `"event"`
    // key, which must
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
/// The canonical shape: state is
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
/// dropped.
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

/// Encode `value` with the canonical JSON dialect and write the
/// `[worker-event] <json>\n` line. Flushes after the newline so a
/// downstream `parse_worker_event` consumer reading line-by-line sees
/// the event the moment it is emitted.
///
/// Short-circuits to a no-op unless `VOICEPI_WORKER_EVENTS` is truthy
/// per [`crate::dictate::env_gates::is_truthy`].
fn write_line<W: Write>(writer: &mut W, value: &Value) -> io::Result<()> {
    if !WorkerEventOutput::Environment.is_enabled() {
        return Ok(());
    }
    writer.write_all(WORKER_EVENT_PREFIX.as_bytes())?;
    let mut serializer = Serializer::with_formatter(&mut *writer, AsciiFormatter::new());
    value.serialize(&mut serializer).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// True when the `VOICEPI_WORKER_EVENTS` env var is truthy (anything
/// but `""`, `"0"`, `"false"`, `"no"`, `"off"` after
/// `.strip().lower()`).
fn worker_events_enabled() -> bool {
    crate::dictate::env_gates::is_truthy(std::env::var(WORKER_EVENTS_ENV).ok().as_deref())
}

/// A `serde_json` formatter that produces the canonical ASCII dialect:
/// every non-ASCII codepoint (and the DEL control, U+007F) becomes a
/// `\uXXXX` escape (lowercase hex), and astral codepoints emit a UTF-16
/// surrogate pair. Float output is re-formatted to the shortest
/// round-trip representation with padded exponents — see
/// [`write_repr_float`].
///
/// Inherits compact separators (`,` / `:`) from [`CompactFormatter`];
/// `serde_json::Map` is `BTreeMap`-backed (no `preserve_order` feature
/// on `serde_json` in this crate), so key order is alphabetical.
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
            // The ASCII dialect escapes everything outside the
            // printable ASCII range 0x20..=0x7E — including DEL (0x7F),
            // which writes as `"\u007f"`. CompactFormatter's
            // `write_string_fragment` would let DEL through verbatim, so
            // we treat 0x7F like the non-ASCII branch below.
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

    // Floats are reformatted to the dialect's repr-float form (see
    // `write_repr_float`): scientific-notation exponents are padded
    // to 2+ digits with an explicit sign (`1e-06`, `1e+16`) and the
    // fixed/scientific switchover happens at |x| < 1e-4 / |x| >= 1e16
    // — both differ from serde_json's default formatter.
    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        write_repr_float(writer, value)
    }
    fn write_f32<W: ?Sized + Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        write_repr_float(writer, value as f64)
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

/// Re-serialize `ch` as the ASCII dialect would: lowercase
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

/// Format `value` in the dialect's repr-float form: the shortest
/// round-trip decimal representation, with fixed vs scientific picked
/// by the boundary |value| < 1e-4 → scientific, |value| >= 1e16 →
/// scientific, else fixed. The scientific form always pads the
/// exponent to at least two digits with an explicit sign (`1e-06`,
/// `1e+16`) and strips any trailing `.0` from the mantissa (`1e-06`
/// not `1.0e-06`).
///
/// serde_json's `CompactFormatter::write_f64` also emits shortest
/// round-trip digits but uses a different boundary (its fixed-point
/// range extends down to ~1e-5) and a minimum-digit exponent (`1e-6`).
/// We therefore take its output and reformat the parts that differ:
///
/// 1. If it's already scientific, just rewrite the exponent.
/// 2. If it's fixed but |value| < 1e-4, convert to scientific
///    by counting the leading zeros after `0.`.
/// 3. Otherwise, pass through unchanged.
///
/// `-0.0` / `+0.0` short-circuit to their literal forms. NaN
/// +/-Infinity hand back to the inner formatter — in practice
/// `serde_json::Number::from_f64` rejects them so the branch is
/// unreachable from `Value`, but keeping parity with serde_json's
/// default keeps any direct serializer caller from seeing surprise
/// behaviour from this formatter alone.
fn write_repr_float<W: ?Sized + Write>(writer: &mut W, value: f64) -> io::Result<()> {
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
        // trailing `.0` from the mantissa (`1e-06`, not `1.0e-06`).
        let mantissa = s[..e_idx].strip_suffix(".0").unwrap_or(&s[..e_idx]);
        let exp: i32 = s[e_idx + 1..].parse().map_err(io::Error::other)?;
        write!(writer, "{}e{:+03}", mantissa, exp)
    } else if abs < 1e-4 {
        // Fixed output but the dialect wants scientific. serde_json's
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
