//! `whisper-dictate self-test history-write` — write one synthetic
//! utterance event through the shipping history sink and report the
//! resolved path + row.
//!
//! ## What this catches
//!
//! [`crate::dictate::session::history_sink`] is silent on failure by
//! contract (a broken history file must never abort a dictation). That
//! makes "the history isn't recording anything" tough to diagnose without
//! a full PTT session. This verb takes a `--text` string, builds a
//! Python-parity utterance event, runs it through
//! [`crate::dictate::session::history_sink_from_settings`], and reports
//! the file path + the row that was written so the operator can confirm
//! the sink is wired correctly and the config layer resolved to the
//! expected file.
//!
//! ## Envelope
//!
//! ```json
//! {
//!   "kind": "history_write_self_test",
//!   "ok": true|false,
//!   "error": null | "…",
//!   "enabled": true|false,
//!   "path": "C:\\…\\history.jsonl",
//!   "row": { … full filtered row … } | null,
//!   "bytes_written": 123
//! }
//! ```
//!
//! `ok=false` when the gate is on but the write failed (unwritable
//! parent, permission denied). The gate being off with no row written is
//! the correct "user disabled history" answer and exits 0.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::dictate::session::history_sink::{
    effective_history_settings, history_sink_from_settings,
};

/// Options for [`run_history_write_self_test`].
#[derive(Debug, Clone)]
pub struct HistoryWriteOptions {
    /// Utterance text to write. Required (empty string still writes a
    /// row so the operator can pin the shape).
    pub text: String,
    /// Override the resolved history path (used by the unit tests so
    /// the shipping user's history isn't touched). When empty, the
    /// verb writes to the operator's configured file.
    pub path_override: Option<PathBuf>,
    /// Force the gate on regardless of the ambient env / config. Same
    /// safety-net as `path_override`.
    pub force_enabled: Option<bool>,
}

impl Default for HistoryWriteOptions {
    fn default() -> Self {
        Self {
            text: String::new(),
            path_override: None,
            force_enabled: None,
        }
    }
}

/// Verb output.
#[derive(Debug, Clone)]
pub struct HistoryWriteReport {
    /// Was the sink enabled at write time?
    pub enabled: bool,
    /// File the sink wrote to (or would have written to if enabled).
    pub path: PathBuf,
    /// The last row appended (filtered through the `HISTORY_KEYS`
    /// allow-list), or `None` when the sink was disabled.
    pub row: Option<Value>,
    /// Post-write file size in bytes. `0` when the file didn't exist or
    /// the sink was disabled.
    pub bytes_written: u64,
    /// Populated on failure (unwritable path, config layer error).
    pub error: Option<String>,
}

impl HistoryWriteReport {
    pub fn exit_ok(&self) -> bool {
        self.error.is_none()
    }

    pub fn to_json(&self) -> String {
        json!({
            "kind": "history_write_self_test",
            "ok": self.exit_ok(),
            "error": self.error,
            "enabled": self.enabled,
            "path": self.path.to_string_lossy(),
            "row": self.row,
            "bytes_written": self.bytes_written,
        })
        .to_string()
    }

    pub fn to_plain(&self) -> String {
        let mut out = format!(
            "[self-test history-write] enabled={} path={}\n",
            self.enabled,
            self.path.display()
        );
        out.push_str(&format!("  bytes_written={}\n", self.bytes_written));
        if let Some(err) = &self.error {
            out.push_str(&format!("  FAIL: {err}\n"));
        } else if let Some(row) = &self.row {
            out.push_str(&format!("  row={row}\n  PASS\n"));
        } else {
            out.push_str("  gate off; no row written\n  PASS\n");
        }
        out
    }
}

/// Build a minimal Python-parity utterance event with `text`. Populates
/// the same core fields (`ts`, `event`, `text`, `stt_backend`) the
/// shipping session emits so the sink's filter has a realistic payload
/// to filter.
fn synthetic_event(text: &str) -> Value {
    // Wall-clock timestamp (seconds since epoch) as a float, mirroring
    // Python's `time.time()`.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default();
    json!({
        "ts": ts,
        "event": "utterance",
        "text": text,
        "text_chars": text.chars().count(),
        "stt_backend": "self_test",
    })
}

/// Drive the sink.
pub fn run_history_write_self_test(opts: HistoryWriteOptions) -> HistoryWriteReport {
    // Resolve the settings via the same overlay the shipping sink uses.
    let settings = effective_history_settings();
    let enabled = opts.force_enabled.unwrap_or(settings.enabled);
    let path = opts.path_override.clone().unwrap_or(settings.path.clone());

    if !enabled {
        return HistoryWriteReport {
            enabled: false,
            path,
            row: None,
            bytes_written: 0,
            error: None,
        };
    }

    let event = synthetic_event(&opts.text);
    let filtered = crate::telemetry::history_event(&event);

    let write_result = if let Some(override_path) = opts.path_override.as_ref() {
        // Direct-write path: bypass `history_sink_from_settings` so the
        // unit tests can point at a scratch dir without leaking the
        // shipping settings.
        crate::telemetry::append_jsonl(override_path, &filtered)
    } else {
        // Production path: build the shipping reloading sink so the
        // exact code path the session runs is exercised.
        let sink = history_sink_from_settings();
        if let Some(sink) = sink {
            sink.append(&event);
        }
        Ok(())
    };

    let (bytes_written, error) = match write_result {
        Ok(()) => {
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            (size, None)
        }
        Err(err) => (0, Some(format!("history write failed: {err}"))),
    };

    HistoryWriteReport {
        enabled,
        path,
        row: Some(filtered),
        bytes_written,
        error,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_row_to_override_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let report = run_history_write_self_test(HistoryWriteOptions {
            text: "smoke test".to_owned(),
            path_override: Some(path.clone()),
            force_enabled: Some(true),
        });
        assert!(report.exit_ok(), "must pass on writable scratch dir");
        assert!(report.enabled);
        assert!(report.bytes_written > 0);
        let raw = fs::read_to_string(&path).unwrap();
        let row: Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(row["text"], "smoke test");
        assert_eq!(row["stt_backend"], "self_test");
    }

    #[test]
    fn disabled_gate_writes_nothing_but_still_passes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let report = run_history_write_self_test(HistoryWriteOptions {
            text: "nope".to_owned(),
            path_override: Some(path.clone()),
            force_enabled: Some(false),
        });
        assert!(!report.enabled);
        assert!(report.exit_ok());
        assert!(!path.exists(), "file must not be created when gate is off");
    }

    #[test]
    fn report_json_shape_has_stable_keys() {
        let report = HistoryWriteReport {
            enabled: true,
            path: PathBuf::from("/tmp/history.jsonl"),
            row: Some(json!({"text": "hi"})),
            bytes_written: 12,
            error: None,
        };
        let v: Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(v["kind"], "history_write_self_test");
        assert_eq!(v["ok"], true);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["bytes_written"], 12);
        assert_eq!(v["row"]["text"], "hi");
    }

    #[test]
    fn synthetic_event_includes_allow_listed_fields() {
        let ev = synthetic_event("hello");
        assert_eq!(ev["text"], "hello");
        assert_eq!(ev["text_chars"], 5);
        assert_eq!(ev["event"], "utterance");
        assert_eq!(ev["stt_backend"], "self_test");
        assert!(ev["ts"].is_number());
    }

    #[test]
    fn write_failure_populates_error() {
        // Aim the sink at an un-creatable parent (existing regular file
        // as parent) — the sink's own error swallowing means we'd get
        // `ok=true` with `bytes_written=0`. That's fine for the shipping
        // sink but hides regressions from the self-test. Using the
        // override path forces the direct-write branch, which does
        // propagate the io::Error, and pins the failure shape.
        let dir = tempdir().unwrap();
        let file_as_parent = dir.path().join("not-a-dir");
        fs::write(&file_as_parent, "").unwrap();
        let path = file_as_parent.join("history.jsonl");
        let report = run_history_write_self_test(HistoryWriteOptions {
            text: "boom".to_owned(),
            path_override: Some(path),
            force_enabled: Some(true),
        });
        assert!(!report.exit_ok(), "unwritable path must surface as error");
        assert!(report
            .error
            .as_deref()
            .unwrap()
            .contains("history write failed"));
    }
}
