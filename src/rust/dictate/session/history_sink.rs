//! History-JSONL sink for [`super::DictateSession`].
//!
//! Ports the WRITE side of `vp_history.py` to Rust so an in-process Rust
//! engine session records every completed utterance to the same local
//! JSONL file the Python engine writes to today (parity blocker #1 for
//! flipping the default engine to Rust — the READ side already lives in
//! [`crate::history`] and honours the same [`crate::telemetry::history_path_from_settings`]
//! path, so writer + reader always agree).
//!
//! # Contract
//!
//! * **Path**: [`crate::telemetry::history_path_from_settings`] --
//!   `settings.history_jsonl` when set (mirrors Python's
//!   `VOICEPI_HISTORY_JSONL` config setting), else the platform default
//!   from [`crate::config::default_history_path`]:
//!   - Windows: `%APPDATA%\WhisperDictate\history.jsonl`
//!   - Linux/macOS: `$XDG_STATE_HOME/whisper-dictate/history.jsonl`
//!     (default `~/.local/state`)
//!   These are the exact same defaults `vp_history.default_history_path`
//!   returns, so a user upgrading from the Python engine keeps appending
//!   to their existing file with no schema break.
//!
//! * **Gate**: `settings.history_enabled` (default `true`, mirrors
//!   Python's `VOICEPI_HISTORY_ENABLED` config setting). When disabled,
//!   [`history_sink_from_settings`] returns `None` so no sink is even
//!   constructed -- the session pays zero per-utterance cost.
//!
//! * **Schema**: [`crate::telemetry::history_event`] filters the utterance
//!   event down to the exact same `HISTORY_KEYS` allow-list Python's
//!   `_history_event` uses, so a Rust-written row is byte-identical to a
//!   Python-written row for the same input event. [`crate::telemetry::append_jsonl`]
//!   handles the file write itself (create parent dirs, open with append,
//!   compact JSON + newline).
//!
//! * **Errors**: non-fatal. A failed write logs one `[history]` warning to
//!   stderr and the session continues -- matching Python's
//!   `_record_utterance_event`, which wraps `append_record_sinks` in
//!   `try / except OSError`. A history-file break can never abort a
//!   dictation.

use std::path::PathBuf;

use serde_json::Value;

use crate::config;
use crate::telemetry;

/// The seam the session calls on every successful utterance. The
/// production impl [`JsonlHistorySink`] writes to the local JSONL file;
/// [`NoopHistorySink`] exists for tests that want the sink attached
/// without touching disk.
pub trait HistorySink {
    /// Record one utterance event. `event` is the payload the session
    /// just emitted on the worker-event stream (already includes `ts`,
    /// `text`, `stt_backend`-family fields the wire layer populates); the
    /// implementation is responsible for the Python-parity field filter
    /// via [`crate::telemetry::history_event`] before writing.
    fn append(&self, event: &Value);
}

/// Writes each utterance to a JSONL file on disk after filtering the event
/// down to the [`crate::telemetry::history_event`] allow-list. Path is
/// resolved at construction and cached; the process env / settings are
/// re-read at construction, not per utterance, so a Settings save requires
/// building a fresh session (matching the current supervisor lifecycle).
pub struct JsonlHistorySink {
    path: PathBuf,
}

impl JsonlHistorySink {
    /// Build a sink that writes to `path`. Prefer [`history_sink_from_settings`]
    /// in production so the gate + path resolution match Python.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The file this sink writes to. Exposed so tests can assert
    /// [`history_sink_from_settings`] picked the expected path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl HistorySink for JsonlHistorySink {
    fn append(&self, event: &Value) {
        let filtered = telemetry::history_event(event);
        if let Err(err) = telemetry::append_jsonl(&self.path, &filtered) {
            // Non-fatal, matching Python's
            // `except OSError: print(f"[sinks] could not write ...")`.
            // The prefix is `[history]` (vs Python's `[sinks]`) because
            // this Rust path only writes the history file (metrics.jsonl
            // stays on the Python engine's `append_record_sinks` path for
            // now); tag it distinctly so log-scrapers can tell the two
            // apart during the migration.
            eprintln!(
                "[history] could not append to {}: {err}",
                self.path.display()
            );
        }
    }
}

/// Discard every event. Useful for tests that want to build a session
/// with the sink seam wired up without touching disk, and for callers
/// that want to explicitly disable history for a single session without
/// changing the global gate.
#[derive(Default)]
pub struct NoopHistorySink;

impl HistorySink for NoopHistorySink {
    fn append(&self, _event: &Value) {}
}

/// Resolve the production history sink from `AppSettings`, matching
/// Python's `vp_history.history_enabled` / `vp_history.history_path`
/// pair:
///
/// * `history_enabled=false` -> `None` (session pays zero cost).
/// * `history_enabled=true` + `history_jsonl` set in config -> sink
///   writing to that path.
/// * `history_enabled=true` + `history_jsonl` unset -> sink writing to
///   [`config::default_history_path`] (the platform default).
///
/// A settings-load failure is treated as "history off" with a warning to
/// stderr, so a corrupt config never blocks the whole session from
/// running (parity with Python, whose config load surfaces to the UI but
/// does not tear the worker down).
pub fn history_sink_from_settings() -> Option<Box<dyn HistorySink + Send>> {
    let settings = match config::load_settings() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("[history] could not load settings; disabling history: {err}");
            return None;
        }
    };
    if !settings.history_enabled {
        return None;
    }
    let path = if settings.history_jsonl.trim().is_empty() {
        config::default_history_path()
    } else {
        PathBuf::from(settings.history_jsonl)
    };
    Some(Box::new(JsonlHistorySink::new(path)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn jsonl_sink_appends_filtered_row_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());

        // Emit an event carrying BOTH allow-listed and non-allow-listed
        // fields; the sink must drop the non-allow-listed ones (parity
        // with Python's `_history_event` filter).
        sink.append(&json!({
            "ts": 1706280000.5,
            "event": "utterance",
            "text": "hello world",
            "stt_backend": "whisper",
            "large_unused_blob": "drop me",
        }));

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.ends_with('\n'), "JSONL row must end with newline");
        let row: Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(row["text"], "hello world");
        assert_eq!(row["stt_backend"], "whisper");
        assert_eq!(row["ts"], 1706280000.5);
        assert!(
            row.get("large_unused_blob").is_none(),
            "non-allow-listed field must be filtered out"
        );
    }

    #[test]
    fn jsonl_sink_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // Path with a not-yet-created parent -- the sink must create it,
        // matching the existing `telemetry::append_jsonl` contract.
        let path = dir.path().join("nested").join("dirs").join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());
        sink.append(&json!({"text": "hi"}));
        assert!(
            path.exists(),
            "sink must create parent directories on first write"
        );
    }

    #[test]
    fn jsonl_sink_appends_multiple_rows_preserving_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());

        sink.append(&json!({"text": "first"}));
        sink.append(&json!({"text": "second"}));
        sink.append(&json!({"text": "third"}));

        let raw = fs::read_to_string(&path).unwrap();
        let texts: Vec<String> = raw
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["text"].to_string())
            .collect();
        assert_eq!(
            texts,
            vec![
                "\"first\"".to_string(),
                "\"second\"".to_string(),
                "\"third\"".to_string()
            ]
        );
    }

    #[test]
    fn jsonl_sink_write_failure_is_non_fatal() {
        // Aim the sink at a path whose parent CANNOT be created (an existing
        // regular file, so `create_dir_all` would fail). The sink must swallow
        // the error -- a dropped dictation would be worse than a missing
        // history row.
        let dir = tempfile::tempdir().unwrap();
        let file_as_parent = dir.path().join("not-a-dir");
        fs::write(&file_as_parent, "").unwrap();
        let path = file_as_parent.join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());
        // Must not panic / propagate.
        sink.append(&json!({"text": "should not blow up"}));
        assert!(
            !path.exists(),
            "the unwritable path should stay non-existent -- \
             the sink swallowed the error"
        );
    }

    #[test]
    fn round_trip_write_then_read_via_history_reader() {
        // The whole point of writing from Rust is that the pre-existing
        // `crate::history` reader sees the row. This exercises the
        // WRITER + READER together.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());

        sink.append(&json!({
            "ts": 1706280000.5,
            "text": "hello from rust",
            "stt_backend": "whisper",
        }));

        let rows = crate::history::read_rows(&path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["text"], "hello from rust");
        assert_eq!(rows[0]["stt_backend"], "whisper");

        let last = crate::history::last_row(&path).unwrap().unwrap();
        assert_eq!(last["text"], "hello from rust");
    }

    #[test]
    fn matches_python_row_shape_for_the_same_event() {
        // Parity fixture: given a Python-shaped utterance event, the Rust
        // writer must produce the SAME JSONL row as Python's
        // `append_record_sinks` path (which itself already goes through the
        // shared `telemetry::history_event` filter today, since the Python
        // helper shells out to the same Rust code). This test pins the
        // schema contract so a future refactor that touches EITHER filter
        // catches drift here.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());

        let event = json!({
            "ts": 1706280000.5,
            "event": "utterance",
            "text": "hej verden",
            "raw_text": "hej verden",
            "text_preview": "hej verden",
            "text_chars": 10,
            "recording_s": 1.23,
            "audio_duration_s": 1.2,
            "compute_s": 0.04,
            "language": "da",
            "stt_backend": "whisper",
            "model": "large-v3-turbo",
            // Non-allow-listed noise that MUST be filtered out:
            "api_key": "secret",
            "raw_pcm_bytes": 12345,
        });
        sink.append(&event);

        let raw = fs::read_to_string(&path).unwrap();
        let row: Value = serde_json::from_str(raw.trim()).unwrap();
        // Every allow-listed field survives with its value.
        for (key, want) in [
            ("ts", &Value::from(1706280000.5)),
            ("event", &Value::from("utterance")),
            ("text", &Value::from("hej verden")),
            ("raw_text", &Value::from("hej verden")),
            ("text_preview", &Value::from("hej verden")),
            ("text_chars", &Value::from(10)),
            ("recording_s", &Value::from(1.23)),
            ("audio_duration_s", &Value::from(1.2)),
            ("compute_s", &Value::from(0.04)),
            ("language", &Value::from("da")),
            ("stt_backend", &Value::from("whisper")),
            ("model", &Value::from("large-v3-turbo")),
        ] {
            assert_eq!(&row[key], want, "field {key} must match");
        }
        // Non-allow-listed fields are gone.
        assert!(row.get("api_key").is_none(), "api_key must be filtered");
        assert!(
            row.get("raw_pcm_bytes").is_none(),
            "raw_pcm_bytes must be filtered"
        );
    }

    #[test]
    fn noop_sink_writes_nothing() {
        // Trivially: the noop sink is a discard, so nothing observable
        // happens. Kept as an executable spec so a future refactor cannot
        // accidentally turn Noop into a fallback that touches disk.
        let sink = NoopHistorySink;
        sink.append(&json!({"text": "ignored"}));
    }
}
