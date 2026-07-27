//! Metrics-JSONL sink for [`super::DictateSession`].
//!
//! Ports the metrics-file WRITE side of Python's
//! `vp_history.append_record_sinks` -- the second sink that
//! `vp_dictate._record_utterance_event` fans the utterance event out to
//! alongside the `_emit_worker_event` stdout emission and the history sink
//! (see [`super::history_sink`]). Closes parity blocker #6 for flipping
//! the default engine to Rust so a user's existing `metrics.jsonl` accepts
//! Rust-written rows without a schema break.
//!
//! # Contract
//!
//! * **Path**: [`AppSettings::metrics_jsonl`] (config key `metrics_jsonl`,
//!   env `VOICEPI_METRICS_JSONL`). Tilde-expanded (`~/foo` ->
//!   `$HOME/foo`) to match Python's `os.path.expanduser(raw_metrics_path)`
//!   in `vp_history.append_record_sinks`. There is NO platform default:
//!   Python only writes the metrics file when the user opts in by setting
//!   the path explicitly, and the Rust port mirrors that -- an unset
//!   `metrics_jsonl` means no metrics file, ever.
//!
//! * **Gate**: BOTH `settings.inject_json` (config key `json_output`,
//!   env `VOICEPI_JSON`) AND a non-empty `metrics_jsonl` are required,
//!   matching Python's `metrics_path = os.path.expanduser(raw) if
//!   json_output and raw_metrics_path else ""`. When either is off
//!   [`metrics_sink_from_settings`] returns `None` so the session pays
//!   zero per-utterance cost. The metrics file is part of the
//!   machine-readable integration surface, so it is only written when the
//!   operator has opted into structured JSON output on stdout -- a
//!   prefilled-but-unused path (the UI suggests `metrics.jsonl` next to
//!   `config.json`) therefore stays inert until "JSON output" is enabled.
//!
//! * **Schema**: the FULL utterance event, unfiltered. Unlike the history
//!   sink (which passes rows through the [`crate::telemetry::history_event`]
//!   allow-list) the metrics file preserves every field the emitter wrote,
//!   matching Python's `append_record_sinks(event, metrics_path=...)`
//!   which calls `_append_jsonl(metrics_path, event)` on the raw dict.
//!   This is deliberate: metrics is the machine-readable timing/quality
//!   feed for external tooling, so it carries everything.
//!
//! * **Errors**: non-fatal. A failed write logs one `[metrics]` warning to
//!   stderr and the session continues -- matching Python's
//!   `_record_utterance_event`, which wraps `append_record_sinks` in
//!   `try / except OSError`. A metrics-file break can never abort a
//!   dictation.

use std::path::PathBuf;

use serde_json::Value;

use crate::config;

/// The seam the session calls on every utterance the wire layer emitted a
/// payload for. The production impl [`JsonlMetricsSink`] writes to the
/// local JSONL file; [`NoopMetricsSink`] exists for tests that want the
/// sink attached without touching disk.
pub trait MetricsSink {
    /// Record one utterance event. `event` is the full payload the
    /// worker-event emitter just wrote (already includes `ts`, `text`,
    /// `stt_backend`-family fields, plus the `post_*` and
    /// `dictionary_replacements` metadata when applicable). The
    /// implementation is responsible for the write itself; unlike
    /// [`super::history_sink::HistorySink`] the metrics file preserves
    /// every field (no allow-list filter).
    fn append(&self, event: &Value);
}

/// Writes each utterance event, unfiltered, to a JSONL file on disk. Path
/// is resolved at construction and cached; the process env / settings are
/// re-read at construction, not per utterance, so a Settings save
/// requires building a fresh session (matching the current supervisor
/// lifecycle).
pub struct JsonlMetricsSink {
    path: PathBuf,
}

impl JsonlMetricsSink {
    /// Build a sink that writes to `path`. Prefer [`metrics_sink_from_settings`]
    /// in production so the gate + path resolution match Python.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The file this sink writes to. Exposed so tests can assert
    /// [`metrics_sink_from_settings`] picked the expected path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl MetricsSink for JsonlMetricsSink {
    fn append(&self, event: &Value) {
        if let Err(err) = crate::telemetry::append_jsonl(&self.path, event) {
            // Non-fatal, matching Python's
            // `except OSError: print(f"[sinks] could not write ...")`.
            // Tag the prefix `[metrics]` (vs the history sink's
            // `[history]`) so log-scrapers can tell the two sinks apart.
            eprintln!(
                "[metrics] could not append to {}: {err}",
                self.path.display()
            );
        }
    }
}

/// Discard every event. Useful for tests that want to build a session
/// with the sink seam wired up without touching disk, and for callers
/// that want to explicitly disable metrics for a single session without
/// changing the global gate.
#[derive(Default)]
pub struct NoopMetricsSink;

impl MetricsSink for NoopMetricsSink {
    fn append(&self, _event: &Value) {}
}

/// Resolve the production metrics sink from `AppSettings`, matching
/// Python's `append_record_sinks(metrics_jsonl=..., json_output=...)`
/// gate:
///
/// * `inject_json=false` -> `None` (the metrics file is off; a
///   prefilled `metrics_jsonl` stays inert).
/// * `metrics_jsonl` empty / whitespace -> `None` (no path, no sink).
/// * both set -> sink writing to the tilde-expanded `metrics_jsonl` path.
///
/// A settings-load failure is treated as "metrics off" with a warning to
/// stderr, so a corrupt config never blocks the whole session from
/// running (parity with Python, whose config load surfaces to the UI but
/// does not tear the worker down).
pub fn metrics_sink_from_settings() -> Option<Box<dyn MetricsSink + Send>> {
    let settings = match config::load_settings() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("[metrics] could not load settings; disabling metrics: {err}");
            return None;
        }
    };
    if !settings.inject_json {
        return None;
    }
    let raw = settings.metrics_jsonl.trim();
    if raw.is_empty() {
        return None;
    }
    Some(Box::new(JsonlMetricsSink::new(expand_user(raw))))
}

/// Expand a leading `~` to the user's home directory, matching Python's
/// `os.path.expanduser`. Anything without a leading `~` is returned as-is.
/// A missing `HOME`/`USERPROFILE` falls through to `.` -- the same
/// last-resort the sibling `dictionary::store::expand_user` and
/// `corpus::expand_tilde` helpers pick.
fn expand_user(raw: &str) -> PathBuf {
    if let Some(stripped) = raw.strip_prefix('~') {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let rest = stripped.trim_start_matches(['/', '\\']);
        if rest.is_empty() {
            return home;
        }
        return home.join(rest);
    }
    PathBuf::from(raw)
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
    fn jsonl_sink_appends_unfiltered_row_to_disk() {
        // The metrics sink is UNFILTERED (unlike history_sink). Every
        // field on the input event must survive to disk -- external
        // tooling relies on the machine-readable metrics file carrying
        // the full per-utterance record (timing + quality + backend
        // metadata + arbitrary future fields).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.jsonl");
        let sink = JsonlMetricsSink::new(path.clone());

        sink.append(&json!({
            "ts": 1706280000.5,
            "event": "utterance",
            "text": "hello world",
            "stt_backend": "whisper",
            "recording_s": 1.23,
            "compute_ms": 42,
            "language": "en",
            "audio_duration_s": 1.2,
            // Arbitrary future field -- metrics must not filter it out.
            "custom_debug_field": {"nested": true},
        }));

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.ends_with('\n'), "JSONL row must end with newline");
        let row: Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(row["text"], "hello world");
        assert_eq!(row["stt_backend"], "whisper");
        assert_eq!(row["recording_s"], 1.23);
        assert_eq!(row["compute_ms"], 42);
        assert_eq!(
            row["custom_debug_field"]["nested"], true,
            "metrics sink must NOT drop unknown fields (schema-open by design)"
        );
    }

    #[test]
    fn jsonl_sink_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // Path with a not-yet-created parent -- the sink must create it,
        // matching the shared `telemetry::append_jsonl` contract.
        let path = dir.path().join("nested").join("dirs").join("metrics.jsonl");
        let sink = JsonlMetricsSink::new(path.clone());
        sink.append(&json!({"text": "hi"}));
        assert!(
            path.exists(),
            "sink must create parent directories on first write"
        );
    }

    #[test]
    fn jsonl_sink_appends_multiple_rows_preserving_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.jsonl");
        let sink = JsonlMetricsSink::new(path.clone());

        sink.append(&json!({"text": "first"}));
        sink.append(&json!({"text": "second"}));
        sink.append(&json!({"text": "third"}));

        let raw = fs::read_to_string(&path).unwrap();
        let texts: Vec<Value> = raw
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["text"].clone())
            .collect();
        assert_eq!(texts, vec![json!("first"), json!("second"), json!("third")]);
    }

    #[test]
    fn jsonl_sink_write_failure_is_non_fatal() {
        // Aim the sink at a path whose parent CANNOT be created (an
        // existing regular file, so `create_dir_all` would fail). The
        // sink must swallow the error -- a dropped dictation would be
        // worse than a missing metrics row.
        let dir = tempfile::tempdir().unwrap();
        let file_as_parent = dir.path().join("not-a-dir");
        fs::write(&file_as_parent, "").unwrap();
        let path = file_as_parent.join("metrics.jsonl");
        let sink = JsonlMetricsSink::new(path.clone());
        // Must not panic / propagate.
        sink.append(&json!({"text": "should not blow up"}));
        assert!(
            !path.exists(),
            "the unwritable path should stay non-existent -- \
             the sink swallowed the error"
        );
    }

    #[test]
    fn round_trip_write_then_read_back_matches_input() {
        // Round-trip: parse the written line back and assert every input
        // field survives. Metrics has no reader like `crate::history`, so
        // the acceptance is a raw JSON round-trip: what we wrote is what a
        // JSON-parsing consumer will see.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.jsonl");
        let sink = JsonlMetricsSink::new(path.clone());

        let event = json!({
            "ts": 1706280000.5,
            "event": "utterance",
            "text": "hej verden",
            "text_chars": 10,
            "recording_s": 1.23,
            "compute_ms": 42,
            "compute_s": 0.04,
            "audio_duration_s": 1.2,
            "language": "da",
            "stt_backend": "whisper",
        });
        sink.append(&event);

        let raw = fs::read_to_string(&path).unwrap();
        let row: Value = serde_json::from_str(raw.trim()).unwrap();
        for key in [
            "ts",
            "event",
            "text",
            "text_chars",
            "recording_s",
            "compute_ms",
            "compute_s",
            "audio_duration_s",
            "language",
            "stt_backend",
        ] {
            assert_eq!(row[key], event[key], "field {key} must round-trip");
        }
    }

    #[test]
    fn noop_sink_writes_nothing() {
        // Trivially: the noop sink is a discard. Kept as an executable
        // spec so a future refactor cannot accidentally turn Noop into
        // a fallback that touches disk.
        let sink = NoopMetricsSink;
        sink.append(&json!({"text": "ignored"}));
    }

    #[test]
    fn expand_user_expands_leading_tilde() {
        // Set a fake HOME so the test is deterministic on any machine.
        // Uses a scratch dir so we don't rely on the real user's home.
        // Take the crate-wide ENV_LOCK: `set_var`/`remove_var` are
        // `unsafe` under Rust 2024 precisely because parallel tests in
        // the same binary observing HOME/USERPROFILE would race here.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let saved_home = std::env::var_os("HOME");
        let saved_userprofile = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", home);
        std::env::set_var("USERPROFILE", home);

        assert_eq!(expand_user("~"), home.to_path_buf());
        assert_eq!(expand_user("~/metrics.jsonl"), home.join("metrics.jsonl"));
        assert_eq!(
            expand_user("/tmp/metrics.jsonl"),
            PathBuf::from("/tmp/metrics.jsonl")
        );

        // Restore env so sibling tests are unaffected.
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
    }
}
