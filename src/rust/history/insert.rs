//! Convert a worker-emitted `utterance` JSON payload into an
//! [`UtteranceRow`] and persist it to the SQLite store.
//!
//! Field extraction is tolerant: every column except `text` and the
//! timestamp pair (`ts` / `ts_unix`) is optional. A missing or
//! wrong-typed field becomes SQL NULL rather than a hard error, so a
//! payload schema drift on the worker side never breaks history
//! recording. The full original payload is also re-serialised into
//! `raw_payload` so the UI can later surface fields we don't index
//! today (and future migrations can backfill new columns from it).

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// Strongly-typed projection of an utterance payload onto the
/// `utterances` table. Public so callers (tests, future history
/// editors) can build rows without going through the JSON path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UtteranceRow {
    pub ts: String,
    pub ts_unix: f64,
    pub text: String,
    pub text_chars: Option<i64>,
    pub stt_backend: Option<String>,
    pub model: Option<String>,
    pub language: Option<String>,
    pub device: Option<String>,
    pub compute_type: Option<String>,
    pub recording_s: Option<f64>,
    pub audio_duration_s: Option<f64>,
    pub compute_s: Option<f64>,
    pub real_time_factor: Option<f64>,
    pub post_processor: Option<String>,
    pub post_mode: Option<String>,
    pub post_model: Option<String>,
    pub post_changed: Option<bool>,
    pub post_error: Option<String>,
    pub gate: Option<String>,
    pub target_title: Option<String>,
    pub target_process: Option<String>,
    pub raw_payload: String,
}

impl UtteranceRow {
    /// Build a row from a worker-emitted JSON payload (the `payload`
    /// of a `RuntimeEvent::Worker` with `event="utterance"`).
    ///
    /// The payload's `ts` field — if present and a number — wins; we
    /// fall back to the system clock so a payload missing a timestamp
    /// still gets persisted in real-time order. Non-object payloads
    /// degrade to a row with empty `text` (a debug breadcrumb rather
    /// than a hard error).
    pub fn from_payload(payload: &Value) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let ts_unix = payload.get("ts").and_then(Value::as_f64).unwrap_or(now);
        let ts = format_iso_utc(ts_unix);
        Self {
            ts,
            ts_unix,
            text: string_field(payload, "text").unwrap_or_default(),
            text_chars: int_field(payload, "text_chars"),
            stt_backend: string_field(payload, "stt_backend"),
            model: string_field(payload, "model"),
            language: string_field(payload, "language"),
            device: string_field(payload, "device"),
            compute_type: string_field(payload, "compute_type"),
            recording_s: float_field(payload, "recording_s"),
            audio_duration_s: float_field(payload, "audio_duration_s"),
            compute_s: float_field(payload, "compute_s"),
            real_time_factor: float_field(payload, "real_time_factor"),
            post_processor: string_field(payload, "post_processor"),
            post_mode: string_field(payload, "post_mode"),
            post_model: string_field(payload, "post_model"),
            post_changed: bool_field(payload, "post_changed"),
            post_error: string_field(payload, "post_error"),
            gate: string_field(payload, "gate"),
            target_title: string_field(payload, "target_title"),
            target_process: string_field(payload, "target_process"),
            raw_payload: payload.to_string(),
        }
    }
}

/// Insert one utterance row. Convenience wrapper around
/// [`insert_row`] that builds the row from a JSON payload first.
/// Returns the new rowid.
pub fn insert_utterance(conn: &Connection, payload: &Value) -> Result<i64> {
    insert_row(conn, &UtteranceRow::from_payload(payload))
}

/// Insert a pre-built [`UtteranceRow`]. Returns the new rowid.
pub fn insert_row(conn: &Connection, row: &UtteranceRow) -> Result<i64> {
    conn.execute(
        "INSERT INTO utterances (\
            ts, ts_unix, text, text_chars, \
            stt_backend, model, language, device, compute_type, \
            recording_s, audio_duration_s, compute_s, real_time_factor, \
            post_processor, post_mode, post_model, post_changed, post_error, \
            gate, target_title, target_process, raw_payload\
         ) VALUES (\
            ?1, ?2, ?3, ?4, \
            ?5, ?6, ?7, ?8, ?9, \
            ?10, ?11, ?12, ?13, \
            ?14, ?15, ?16, ?17, ?18, \
            ?19, ?20, ?21, ?22\
         )",
        params![
            row.ts,
            row.ts_unix,
            row.text,
            row.text_chars,
            row.stt_backend,
            row.model,
            row.language,
            row.device,
            row.compute_type,
            row.recording_s,
            row.audio_duration_s,
            row.compute_s,
            row.real_time_factor,
            row.post_processor,
            row.post_mode,
            row.post_model,
            row.post_changed.map(|b| if b { 1_i64 } else { 0 }),
            row.post_error,
            row.gate,
            row.target_title,
            row.target_process,
            row.raw_payload,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn string_field(payload: &Value, key: &str) -> Option<String> {
    let raw = payload.get(key)?.as_str()?.trim();
    (!raw.is_empty()).then(|| raw.to_owned())
}

fn float_field(payload: &Value, key: &str) -> Option<f64> {
    let value = payload.get(key)?;
    if let Some(raw) = value.as_f64() {
        return Some(raw);
    }
    value.as_str()?.trim().parse::<f64>().ok()
}

fn int_field(payload: &Value, key: &str) -> Option<i64> {
    let value = payload.get(key)?;
    if let Some(raw) = value.as_i64() {
        return Some(raw);
    }
    if let Some(raw) = value.as_f64() {
        return Some(raw as i64);
    }
    value.as_str()?.trim().parse::<i64>().ok()
}

fn bool_field(payload: &Value, key: &str) -> Option<bool> {
    let value = payload.get(key)?;
    if let Some(raw) = value.as_bool() {
        return Some(raw);
    }
    if let Some(raw) = value.as_i64() {
        return Some(raw != 0);
    }
    value.as_str()?.trim().parse::<bool>().ok()
}

/// Format a unix-epoch (seconds) as an ISO-8601 UTC timestamp.
///
/// Avoids pulling in `chrono` for one helper — the Python worker side
/// emits the same `YYYY-MM-DDTHH:MM:SS.sssZ` shape, so keeping this in
/// pure Rust lets unit tests pin the bytes without a new dep.
fn format_iso_utc(unix_seconds: f64) -> String {
    // Round-trip via integer seconds + milliseconds so we don't print
    // 17-digit floating noise. Negative timestamps fall through to a
    // best-effort "1970-01-01T00:00:00.000Z" since none of our worker
    // payloads can produce a pre-epoch ts in practice.
    // Round-trip the entire value through total-milliseconds so a
    // fractional part that rounds up to 1000 ms carries into the next
    // second instead of producing an out-of-range ".1000Z" suffix.
    // Without this, a payload like 1700000000.9999999 lands as
    // 22:13:20.1000Z (RFC 3339 violation); after, it lands as
    // 22:13:21.000Z. Claude P1 review on PR #427.
    let total_ms = (unix_seconds.max(0.0) * 1000.0).round() as i64;
    let secs = total_ms / 1000;
    let millis = total_ms % 1000;
    let (year, month, day, hour, minute, second) = civil_from_unix(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Howard Hinnant's days-from-civil algorithm, inverted for unix
/// seconds → (year, month, day, hour, minute, second). Public-domain
/// algorithm, pure integer arithmetic — no chrono/time crate needed.
fn civil_from_unix(unix_seconds: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = unix_seconds.div_euclid(86_400);
    let secs_of_day = unix_seconds.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    // Shift epoch from 1970-01-01 to 0000-03-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y: i64 = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year as i32, month, day, hour, minute, second)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        crate::history::open_in_memory().unwrap()
    }

    #[test]
    fn from_payload_extracts_known_fields() {
        let payload = serde_json::json!({
            "text": "hello world",
            "text_chars": 11,
            "stt_backend": "whisper",
            "model": "small.en",
            "language": "en",
            "device": "cpu",
            "compute_type": "int8",
            "recording_s": 1.5,
            "audio_duration_s": 1.6,
            "compute_s": 0.8,
            "real_time_factor": 2.0,
            "post_processor": "none",
            "post_mode": "off",
            "post_model": null,
            "post_changed": false,
            "post_error": null,
            "gate": null,
            "target_title": " Editor ",
            "target_process": "code.exe",
            "ts": 1_700_000_000.5_f64,
        });
        let row = UtteranceRow::from_payload(&payload);
        assert_eq!(row.text, "hello world");
        assert_eq!(row.text_chars, Some(11));
        assert_eq!(row.stt_backend.as_deref(), Some("whisper"));
        assert_eq!(row.model.as_deref(), Some("small.en"));
        assert_eq!(row.language.as_deref(), Some("en"));
        assert_eq!(row.device.as_deref(), Some("cpu"));
        assert_eq!(row.compute_type.as_deref(), Some("int8"));
        assert_eq!(row.recording_s, Some(1.5));
        assert_eq!(row.audio_duration_s, Some(1.6));
        assert_eq!(row.compute_s, Some(0.8));
        assert_eq!(row.real_time_factor, Some(2.0));
        assert_eq!(row.post_processor.as_deref(), Some("none"));
        assert_eq!(row.post_changed, Some(false));
        assert_eq!(row.post_error, None);
        assert_eq!(row.target_title.as_deref(), Some("Editor"));
        assert_eq!(row.target_process.as_deref(), Some("code.exe"));
        assert!(row.ts.starts_with("2023-11-14T"));
        assert!((row.ts_unix - 1_700_000_000.5).abs() < 1e-3);
    }

    #[test]
    fn from_payload_uses_system_clock_when_ts_absent() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let row = UtteranceRow::from_payload(&serde_json::json!({"text": "hi"}));
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert!(row.ts_unix >= before - 0.5 && row.ts_unix <= after + 0.5);
    }

    #[test]
    fn from_payload_tolerates_non_object() {
        let row = UtteranceRow::from_payload(&serde_json::json!("oops"));
        assert_eq!(row.text, "");
        assert_eq!(row.stt_backend, None);
    }

    #[test]
    fn from_payload_coerces_numeric_strings() {
        let payload = serde_json::json!({
            "text": "x",
            "recording_s": "1.25",
            "text_chars": "42",
            "post_changed": "true"
        });
        let row = UtteranceRow::from_payload(&payload);
        assert_eq!(row.recording_s, Some(1.25));
        assert_eq!(row.text_chars, Some(42));
        assert_eq!(row.post_changed, Some(true));
    }

    #[test]
    fn insert_utterance_persists_row_and_returns_rowid() {
        let conn = fresh();
        let id = insert_utterance(
            &conn,
            &serde_json::json!({"text": "first", "stt_backend": "whisper"}),
        )
        .unwrap();
        assert_eq!(id, 1);
        let stored: (String, String) = conn
            .query_row(
                "SELECT text, stt_backend FROM utterances WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored.0, "first");
        assert_eq!(stored.1, "whisper");
    }

    #[test]
    fn insert_persists_raw_payload_for_forward_compat() {
        let conn = fresh();
        let payload = serde_json::json!({
            "text": "x",
            "future_field": {"nested": [1, 2, 3]}
        });
        let id = insert_utterance(&conn, &payload).unwrap();
        let raw: String = conn
            .query_row(
                "SELECT raw_payload FROM utterances WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        let restored: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(restored["future_field"]["nested"][2], 3);
    }

    #[test]
    fn insert_via_fts_trigger_populates_search_mirror() {
        let conn = fresh();
        insert_utterance(&conn, &serde_json::json!({"text": "needle in haystack"})).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM utterances_fts WHERE utterances_fts MATCH 'needle'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn format_iso_utc_matches_known_timestamps() {
        // 0 → unix epoch
        assert_eq!(format_iso_utc(0.0), "1970-01-01T00:00:00.000Z");
        // 1_700_000_000 = 2023-11-14T22:13:20 UTC
        assert_eq!(format_iso_utc(1_700_000_000.0), "2023-11-14T22:13:20.000Z");
        // Milliseconds round trip.
        assert_eq!(
            format_iso_utc(1_700_000_000.123),
            "2023-11-14T22:13:20.123Z"
        );
    }

    #[test]
    fn format_iso_utc_handles_near_whole_second_rounding() {
        // Claude P1 review on PR #427: a fractional part that rounds
        // to 1000 ms must carry into the next second instead of
        // producing a 4-digit `.1000Z` field. Python's `time.time()`
        // can emit sub-microsecond precision, so 1_700_000_000.9999999
        // is a payload-reachable value.
        assert_eq!(
            format_iso_utc(1_700_000_000.9999999),
            "2023-11-14T22:13:21.000Z",
            "fractional rollover must roll the second forward, not emit .1000Z",
        );
        // Just below the rollover stays in the lower second.
        assert_eq!(
            format_iso_utc(1_700_000_000.999),
            "2023-11-14T22:13:20.999Z",
        );
    }
}
