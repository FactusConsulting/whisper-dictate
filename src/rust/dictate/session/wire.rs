//! Worker-event line emitters used by [`super::DictateSession`].
//!
//! Writes the narrow `[worker-event] {…}\n` lines the session needs
//! (state strings + reason tokens — all ASCII) inline; richer emissions
//! go through [`crate::dictate::events`].

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::types::{SessionConfig, SessionError, TranscribeResult};
use crate::platform::foreground_window::WindowInfo;

/// Compact-text helper for `text_preview`: collapse whitespace runs to
/// single spaces, then truncate at `limit` (append `...` when clipped).
/// Kept local so wire.rs owns the whole utterance payload shape.
pub(super) const TEXT_PREVIEW_LIMIT: usize = 240;

pub(super) fn compact_text(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TEXT_PREVIEW_LIMIT {
        return collapsed;
    }
    // Truncate on a char boundary and add "..." (3 chars) so the total
    // never exceeds `TEXT_PREVIEW_LIMIT` visible chars.
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

/// Emit one `[worker-event] {…,"event":"status",…}` line. Null and
/// empty-string optional fields are dropped (extras here carry `""` as
/// the "no value" sentinel for capture_backend / audio_device, matching
/// the default `SessionConfig`).
pub(super) fn emit_status_with_output<W: Write>(
    writer: &mut W,
    state: &str,
    extras: &[(&str, Value)],
    output: crate::dictate::events::WorkerEventOutput,
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
    write_line(writer, &Value::Object(payload), output)
}

/// Post-processing artifacts bundled with an `emit_utterance`
/// `build_utterance_payload` call. Grouping these three related pieces
/// (inject outcome + post-processing pass + dictionary rewrite list)
/// keeps the wire emitter under clippy's `too_many_arguments` limit
/// without changing what any field carries.
#[derive(Debug, Clone)]
pub(super) struct UtterancePost<'a> {
    /// Injection failure text (`inject_error` field); `None` on the
    /// success path.
    pub inject_error: Option<String>,
    /// Post-processing outcome (`post_*` fields), `None` when no pass
    /// ran for this utterance.
    pub post: Option<&'a super::PostProcessOutcome>,
    /// Dictionary replacement records emitted as `dictionary_replacements`
    /// when non-empty.
    pub replacements: &'a [crate::dictionary::ReplacementChange],
}

/// Extra context bundled with an `emit_utterance` call so the payload
/// carries every field the utterance event exposes. Kept as a small
/// parameter struct so the callsite is readable.
#[derive(Debug, Clone)]
pub(super) struct UtteranceExtras<'a> {
    /// Post-dictionary, pre-postprocess text (`source_text` ->
    /// `dictionary_text`). Falls back to the final injected text when
    /// the dictionary made no rewrite.
    pub dictionary_text: &'a str,
    /// Foreground-window snapshot captured at [`super::DictateSession::start`]
    /// via the profile matcher's probe. Emits title, process, and the
    /// optional platform target id when present; the id is transient and is
    /// used only to restore the captured window for reinjection.
    pub window: Option<&'a WindowInfo>,
    /// Name of the profile that fired for this utterance. Emitted as
    /// `profile` when present; empty string is dropped.
    pub profile: Option<&'a str>,
    /// Session-level backend metadata (`stt_backend`, `model`, `device`,
    /// `compute_type`, `inject_mode`). Empty strings on any field
    /// are dropped so a bare-Default session (unit tests) still emits
    /// a clean payload.
    pub config: &'a SessionConfig,
    /// Exact loss counters from this capture; None for sessions without capture.
    pub audio_loss: Option<super::audio_loss::RecordingAudioLoss>,
}

pub(super) struct UtteranceEmission<'a> {
    pub run_command_hook: bool,
    /// `None` preserves the process-boundary env/config resolver. Native
    /// in-process sessions pass owned settings explicitly.
    pub command_hook_settings: Option<(&'a str, u64)>,
    pub output: crate::dictate::events::WorkerEventOutput,
}

/// Emit one `[worker-event] {…,"event":"utterance",…}` line and return the
/// serialised payload so the session can also hand it to the history sink.
///
/// Carries the full field set the utterance event exposes from the
/// trait surface, plus the `post_*` post-processing metadata when a
/// pass ran (`post` is `Some`).
pub(super) fn emit_utterance_with_output<W: Write>(
    writer: &mut W,
    text: &str,
    result: &TranscribeResult,
    recording_s: Value,
    post: UtterancePost<'_>,
    extras: UtteranceExtras<'_>,
    emission: UtteranceEmission<'_>,
) -> Result<Value, SessionError> {
    let mut payload = build_utterance_payload(text, result, recording_s, post, extras);
    if emission.run_command_hook {
        annotate_command_hook_with_settings(&mut payload, emission.command_hook_settings);
    }
    write_line(writer, &payload, emission.output)?;
    Ok(payload)
}

/// Assemble the utterance payload without emitting it. Split out so the
/// session can build the payload once and share it with both the wire-format
/// emitter and the history sink, both of which consume the same payload.
pub(super) fn build_utterance_payload(
    text: &str,
    result: &TranscribeResult,
    recording_s: Value,
    post_bundle: UtterancePost<'_>,
    extras: UtteranceExtras<'_>,
) -> Value {
    let UtterancePost {
        inject_error,
        post,
        replacements,
    } = post_bundle;
    let mut payload: Map<String, Value> = Map::new();
    // `ts` is stamped as float seconds since the Unix epoch. Without
    // it, history rows written from the Rust engine lack a timestamp
    // and the `history list` renderer shows blank leading timestamps. `SystemTime::now()` failure (clock before
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
    // `text_preview`: whitespace-collapsed
    // + 240-char truncated form. Emitted verbatim on the utterance event so
    // metrics/history rows carry a display-safe short form.
    payload.insert("text_preview".into(), Value::from(compact_text(text)));
    // `raw_text` (`result.raw_text or source_text`): the
    // backend's untouched decoded text; falls back to the post-dictionary
    // form so metrics never carry an empty string when the backend does
    // not surface a separate raw copy.
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
    // `dictionary_text` (`source_text`): text AFTER the
    // dictionary rewrite pass but BEFORE post-processing / format
    // commands. Falls back to the final text when the dictionary was a
    // passthrough.
    let dictionary_text = if extras.dictionary_text.is_empty() {
        text
    } else {
        extras.dictionary_text
    };
    payload.insert(
        "dictionary_text".into(),
        Value::from(dictionary_text.to_owned()),
    );
    // Clip duration in seconds (rounded to 2 dp). Consumers like
    // `src/rust/ui/log_render.rs` + `src/rust/telemetry.rs` read it.
    payload.insert("recording_s".into(), recording_s);
    if let Some(loss) = extras.audio_loss {
        for (key, count) in loss.fields() {
            payload.insert(key.into(), Value::from(count));
        }
    }
    payload.insert("compute_ms".into(), Value::from(result.latency_ms));
    // `compute_s` is the seconds-rounded mirror of `compute_ms` that
    // existing consumers (`src/rust/ui/log_render.rs` +
    // `src/rust/telemetry.rs`) still read; it is written alongside the
    // milliseconds field so utterances keep their compute time in the
    // UI/history.
    payload.insert(
        "compute_s".into(),
        serde_json::json!(round2(result.latency_ms as f64 / 1000.0)),
    );
    payload.insert(
        "audio_duration_s".into(),
        serde_json::json!(round2(result.duration_s)),
    );
    // `real_time_factor` is the
    // compute-to-audio ratio -- speed of transcription relative to how
    // long the audio was. Emitted as 0.0 when the audio duration is
    // zero (silent buffer, empty capture) so downstream tooling never
    // divides by zero.
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
        // `language_probability` (`result.language_probability`).
        // Only emitted when the backend surfaced a score.
        payload.insert(
            "language_probability".into(),
            serde_json::json!(round2(result.language_probability)),
        );
    }
    // Session-level backend metadata. Each field is dropped when empty so a
    // bare-Default `SessionConfig` (unit tests) still yields a clean
    // payload; production wiring in `rust_session_real_backends.rs`
    // populates the values from the process env / resolved backend.
    insert_non_empty(&mut payload, "stt_backend", &extras.config.stt_backend);
    insert_non_empty(&mut payload, "model", &extras.config.model);
    insert_non_empty(&mut payload, "device", &extras.config.device);
    insert_non_empty(&mut payload, "compute_type", &extras.config.compute_type);
    insert_non_empty(&mut payload, "inject_mode", &extras.config.inject_mode);
    // Provenance (`crate::dictate::provenance`): which runtime served the
    // utterance, which transcription implementation ran, and which compute
    // path it actually used. Every OTHER field above is the *configuration*
    // -- `stt_backend` is the selected backend name and `device` is usually
    // `auto` -- so a record carrying only those cannot distinguish Rust
    // in-process whisper.cpp on Vulkan from a silent CPU fallback.
    // `engine` comes from the session config (fixed per runtime);
    // `stt_impl` / `stt_accel` come from the backend's own report on THIS
    // pass, never from config.
    insert_non_empty(&mut payload, "engine", &extras.config.engine);
    insert_non_empty(&mut payload, "stt_impl", &result.stt_impl);
    insert_non_empty(&mut payload, "stt_accel", &result.stt_accel);
    // Target-window info + profile name. Each field is
    // dropped when empty so a session without a profile matcher (unit
    // tests, `simulate-session`) emits nothing here.
    if let Some(window) = extras.window {
        if let Some(title) = window.title.as_deref() {
            insert_non_empty(&mut payload, "target_title", title);
        }
        if let Some(process) = window.process.as_deref() {
            insert_non_empty(&mut payload, "target_process", process);
        }
        if let Some(target_id) = window.target_id.as_deref() {
            insert_non_empty(&mut payload, "target_id", target_id);
        }
    }
    if let Some(profile) = extras.profile {
        insert_non_empty(&mut payload, "profile", profile);
    }
    if let Some(err) = inject_error {
        payload.insert("inject_error".into(), Value::from(err));
    }
    // Post-processing metadata (only when a pass ran) so
    // `ui/log_render.rs::post_processing_summary` shows the active
    // provider/mode and telemetry/history record latency + failures.
    if let Some(p) = post {
        payload.insert("post_processor".into(), Value::from(p.processor.clone()));
        payload.insert("post_mode".into(), Value::from(p.mode.clone()));
        payload.insert("post_model".into(), Value::from(p.model.clone()));
        payload.insert("post_latency_ms".into(), Value::from(p.latency_ms));
        payload.insert("post_changed".into(), Value::from(p.changed));
        payload.insert("post_fallback".into(), Value::from(p.fallback));
        // `error` is dropped when empty so the UI does not render a
        // blank error.
        if !p.error.is_empty() {
            payload.insert("post_error".into(), Value::from(p.error.clone()));
        }
        // Redaction provenance (public-safe: placeholder / kind / char
        // count only). Always emitted when a pass ran -- `post_redactions`
        // is `[]` when nothing was redacted.
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
    // Dictionary replacements that fired (`dictionary_replacements`),
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
    if let Some(terms) = result.dictionary_terms.as_deref() {
        payload.insert("dictionary_terms".into(), Value::from(terms.to_vec()));
    }
    Value::Object(payload)
}

/// Run the configured hook with the completed utterance as stdin and
/// attach the command-hook result fields.
#[cfg(test)]
pub(super) fn annotate_command_hook(payload: &mut Value) {
    annotate_command_hook_with_settings(payload, None);
}

fn annotate_command_hook_with_settings(payload: &mut Value, settings: Option<(&str, u64)>) {
    let result = match settings {
        Some((command, timeout_ms)) => {
            crate::command_hook::run_command_hook_with_settings(payload, command, timeout_ms)
        }
        None => crate::command_hook::run_command_hook(payload),
    };
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert("command_hook_enabled".into(), Value::from(result.enabled));
    if !result.command.trim().is_empty() {
        object.insert("command_hook_command".into(), Value::from(result.command));
    }
    if let Some(returncode) = result.returncode {
        object.insert("command_hook_returncode".into(), Value::from(returncode));
    }
    object.insert(
        "command_hook_latency_ms".into(),
        Value::from(u64::try_from(result.latency_ms).unwrap_or(u64::MAX)),
    );
    object.insert("command_hook_timeout".into(), Value::from(result.timeout));
    if let Some(error) = result.error {
        crate::diag::log!("[hook] {error}");
        object.insert("command_hook_error".into(), Value::from(error));
    }
}

/// Insert `value` into `payload` under `key` iff the string is non-empty
/// after trimming. Blank fields are dropped so payloads stay clean.
fn insert_non_empty(payload: &mut Map<String, Value>, key: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    payload.insert(key.to_owned(), Value::from(value.to_owned()));
}

/// True for `Value::Null` and the empty-string case, both of which
/// the emitter drops.
fn is_droppable(value: &Value) -> bool {
    if value.is_null() {
        return true;
    }
    if let Value::String(s) = value {
        return s.is_empty();
    }
    false
}

fn write_line<W: Write>(
    writer: &mut W,
    value: &Value,
    output: crate::dictate::events::WorkerEventOutput,
) -> Result<(), SessionError> {
    // Honour the VOICEPI_WORKER_EVENTS env gate (`events::write_line`
    // enforces the same gate). Without it any session run outside the
    // RuntimeSupervisor (e.g. a CLI smoke or tooling integration) would
    // leak lines to whatever writer was passed in.
    if !output.is_enabled() {
        return Ok(());
    }
    writer.write_all(WORKER_EVENT_PREFIX.as_bytes())?;
    // ASCII-escape non-ASCII payload bytes so the worker-event line is
    // safe on Windows shells / hidden subprocess pipes with non-UTF-8
    // code pages. The existing
    // `test_worker_event_drops_none_fields_and_ascii_encodes`
    // characterisation test pins the shape.
    //
    // Implementation: serialise to a String first (serde_json's compact
    // form), then walk codepoint-by-codepoint replacing anything >=
    // U+0080 with a `\uXXXX` BMP escape or a UTF-16 surrogate pair for
    // astral. `events::AsciiFormatter` does the same inside a
    // `serde_json::Formatter` impl.
    let serialised = serde_json::to_string(value).map_err(|e| SessionError::Io(e.to_string()))?;
    write_ascii_escaped(writer, &serialised)?;
    writer.write_all(b"\n")?;
    // Flush so status lines never sit in a buffered writer past the
    // moment the UI needs them.
    writer.flush()?;
    Ok(())
}

fn write_ascii_escaped<W: Write>(writer: &mut W, input: &str) -> Result<(), SessionError> {
    let mut buf = String::with_capacity(input.len());
    for ch in input.chars() {
        let cp = ch as u32;
        // Treat DEL (U+007F) as non-ASCII for escaping purposes — it
        // emits as `\u007f`, matching `events::AsciiFormatter` in this
        // crate.
        // Without this branch a dictated string / device label / error
        // message carrying DEL would land as a raw control byte in the
        // worker-event stream and break consumers on shells with
        // non-UTF-8 code pages. wire.rs:146 (round 3).
        if cp < 0x80 && cp != 0x7f {
            buf.push(ch);
            continue;
        }
        if cp < 0x10000 {
            // BMP codepoint: single `\uXXXX` escape (lowercase hex).
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

/// Round to 2 decimal places. Kept local to avoid a dep on a numeric
/// crate.
pub(super) fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
