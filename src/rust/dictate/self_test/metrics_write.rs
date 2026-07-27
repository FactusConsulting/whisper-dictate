//! `whisper-dictate self-test metrics-write` — write one synthetic
//! utterance event through the shipping metrics sink and report the
//! resolved path + row.
//!
//! ## What this catches
//!
//! [`crate::dictate::session::metrics_sink`] has a two-part gate
//! (`json_output` AND a non-empty `metrics_jsonl`) that a user commonly
//! misconfigures — the UI suggests a file path but leaves `json_output`
//! off, and no metrics ever land on disk. Since the sink swallows all
//! errors by design, this is invisible without a full live session.
//! This verb takes the same `--text` shape as the history-write sibling
//! and reports whether the gate resolved to a writable target.
//!
//! ## Envelope
//!
//! ```json
//! {
//!   "kind": "metrics_write_self_test",
//!   "ok": true|false,
//!   "error": null | "…",
//!   "enabled": true|false,
//!   "path": "C:\\…\\metrics.jsonl" | null,
//!   "row": { … full unfiltered row … } | null,
//!   "bytes_written": 123
//! }
//! ```
//!
//! `enabled=false` with `path=null` is the correct "user did not opt into
//! JSON output" answer and exits 0. `ok=false` only when the gate is on
//! but the write actually failed.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::dictate::session::metrics_sink::{effective_metrics_settings, ReloadingMetricsSink};

/// Options for [`run_metrics_write_self_test`].
#[derive(Debug, Clone, Default)]
pub struct MetricsWriteOptions {
    /// Utterance text to write.
    pub text: String,
    /// Override the resolved metrics path (used by the unit tests so
    /// the shipping user's file isn't touched). Also forces the gate on
    /// so a caller that specifies `path_override` doesn't need to also
    /// export `VOICEPI_JSON=1`.
    pub path_override: Option<PathBuf>,
}

/// Verb output.
#[derive(Debug, Clone)]
pub struct MetricsWriteReport {
    /// Was the sink enabled at write time?
    pub enabled: bool,
    /// Resolved path (or the override). `None` when the sink was
    /// disabled — mirrors Python's "no path" branch.
    pub path: Option<PathBuf>,
    /// Full utterance event as written (no allow-list filter — metrics
    /// is schema-open by design).
    pub row: Option<Value>,
    /// Post-write file size in bytes.
    pub bytes_written: u64,
    /// Populated on failure.
    pub error: Option<String>,
}

impl MetricsWriteReport {
    pub fn exit_ok(&self) -> bool {
        self.error.is_none()
    }

    pub fn to_json(&self) -> String {
        let path_json = self
            .path
            .as_ref()
            .map(|p| Value::from(p.to_string_lossy().to_string()))
            .unwrap_or(Value::Null);
        json!({
            "kind": "metrics_write_self_test",
            "ok": self.exit_ok(),
            "error": self.error,
            "enabled": self.enabled,
            "path": path_json,
            "row": self.row,
            "bytes_written": self.bytes_written,
        })
        .to_string()
    }

    pub fn to_plain(&self) -> String {
        let mut out = format!(
            "[self-test metrics-write] enabled={} path={}\n",
            self.enabled,
            self.path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<none>".to_owned())
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

/// Build a rich metrics event with `text` — includes both the history-allow-
/// listed fields and a few metrics-only ones (latency, compute_ms, custom
/// debug field) so the operator can eyeball that unfiltered fields survive
/// the write. Mirrors what the shipping wire emitter puts on the metrics
/// row.
fn synthetic_event(text: &str) -> Value {
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
        "recording_s": 0.5,
        "audio_duration_s": 0.5,
        "compute_ms": 1,
        "language": "en",
        // Schema-open marker: metrics must NOT filter this out.
        "self_test_marker": true,
    })
}

/// Drive the sink.
pub fn run_metrics_write_self_test(opts: MetricsWriteOptions) -> MetricsWriteReport {
    let event = synthetic_event(&opts.text);

    if let Some(path) = opts.path_override.clone() {
        // Direct-write branch (test / operator override): bypass the
        // gate so a scratch dir doesn't need env vars to work.
        let write = crate::telemetry::append_jsonl(&path, &event);
        let (bytes_written, error) = match write {
            Ok(()) => (fs::metadata(&path).map(|m| m.len()).unwrap_or(0), None),
            Err(err) => (0, Some(format!("metrics write failed: {err}"))),
        };
        return MetricsWriteReport {
            enabled: true,
            path: Some(path),
            row: Some(event),
            bytes_written,
            error,
        };
    }

    // Production path: honour the shipping gate.
    let settings = effective_metrics_settings();
    let Some(settings) = settings else {
        return MetricsWriteReport {
            enabled: false,
            path: None,
            row: None,
            bytes_written: 0,
            error: None,
        };
    };
    let path = settings.path.clone();
    // Exercise the shipping reloading sink via its result-returning
    // variant so a failure surfaces as `ok=false` in the JSON envelope.
    // Codex P2 #621 metrics_write.rs:186: pre-fix, this branch called
    // `sink.append(&event)` (which swallows all errors) and then
    // hard-coded `error: None`, so a broken metrics file was invisible
    // to the self-test AND the Wayland smoke script.
    let (bytes_written, error) = match ReloadingMetricsSink::new().append_with_result(&event) {
        Ok(_) => (fs::metadata(&path).map(|m| m.len()).unwrap_or(0), None),
        Err(err) => (0, Some(format!("metrics write failed: {err}"))),
    };
    MetricsWriteReport {
        enabled: true,
        path: Some(path),
        row: Some(event),
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
    fn writes_unfiltered_row_to_override_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("metrics.jsonl");
        let report = run_metrics_write_self_test(MetricsWriteOptions {
            text: "smoke test".to_owned(),
            path_override: Some(path.clone()),
        });
        assert!(report.exit_ok(), "must pass on writable scratch dir");
        assert!(report.enabled);
        assert!(report.bytes_written > 0);
        let raw = fs::read_to_string(&path).unwrap();
        let row: Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(row["text"], "smoke test");
        // Metrics is schema-open — the marker must survive.
        assert_eq!(row["self_test_marker"], true);
        assert_eq!(row["compute_ms"], 1);
    }

    #[test]
    fn report_json_shape_has_stable_keys() {
        let report = MetricsWriteReport {
            enabled: true,
            path: Some(PathBuf::from("/tmp/metrics.jsonl")),
            row: Some(json!({"text": "hi"})),
            bytes_written: 12,
            error: None,
        };
        let v: Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(v["kind"], "metrics_write_self_test");
        assert_eq!(v["ok"], true);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["bytes_written"], 12);
        assert_eq!(v["row"]["text"], "hi");
    }

    #[test]
    fn disabled_report_has_null_path_and_row() {
        let report = MetricsWriteReport {
            enabled: false,
            path: None,
            row: None,
            bytes_written: 0,
            error: None,
        };
        let v: Value = serde_json::from_str(&report.to_json()).unwrap();
        assert!(v["path"].is_null());
        assert!(v["row"].is_null());
        assert_eq!(v["enabled"], false);
        // Disabled + no error is still ok — matches the shipping "user
        // didn't opt in" branch.
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn write_failure_populates_error() {
        let dir = tempdir().unwrap();
        let file_as_parent = dir.path().join("not-a-dir");
        fs::write(&file_as_parent, "").unwrap();
        let path = file_as_parent.join("metrics.jsonl");
        let report = run_metrics_write_self_test(MetricsWriteOptions {
            text: "boom".to_owned(),
            path_override: Some(path),
        });
        assert!(!report.exit_ok());
        assert!(report
            .error
            .as_deref()
            .unwrap()
            .contains("metrics write failed"));
    }

    /// The production branch (no `path_override`) must also propagate
    /// I/O failures. Pre-fix (Codex P2 #621 metrics_write.rs:186) the
    /// verb hard-coded `error: None` even when the sink swallowed a
    /// write failure, so the JSON envelope reported `ok=true` while
    /// nothing landed on disk.
    #[test]
    fn write_failure_in_production_branch_propagates_from_reloading_sink() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let dir = tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        fs::write(&cfg, "{}").unwrap();
        let file_as_parent = dir.path().join("not-a-dir");
        fs::write(&file_as_parent, "").unwrap();
        let unwritable = file_as_parent.join("metrics.jsonl");

        let saved_cfg = std::env::var_os("VOICEPI_CONFIG");
        let saved_json = std::env::var_os("VOICEPI_JSON");
        let saved_path = std::env::var_os("VOICEPI_METRICS_JSONL");
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::set_var("VOICEPI_JSON", "1");
        std::env::set_var("VOICEPI_METRICS_JSONL", &unwritable);

        let report = run_metrics_write_self_test(MetricsWriteOptions {
            text: "prod-boom".to_owned(),
            path_override: None,
        });

        match saved_cfg {
            Some(v) => std::env::set_var("VOICEPI_CONFIG", v),
            None => std::env::remove_var("VOICEPI_CONFIG"),
        }
        match saved_json {
            Some(v) => std::env::set_var("VOICEPI_JSON", v),
            None => std::env::remove_var("VOICEPI_JSON"),
        }
        match saved_path {
            Some(v) => std::env::set_var("VOICEPI_METRICS_JSONL", v),
            None => std::env::remove_var("VOICEPI_METRICS_JSONL"),
        }

        assert!(
            !report.exit_ok(),
            "the production branch (no path_override) MUST surface \
             the shipping sink's I/O failures; pre-fix this returned \
             ok=true because the trait `append` swallowed the error"
        );
        assert!(
            report
                .error
                .as_deref()
                .map(|e| e.contains("metrics write failed"))
                .unwrap_or(false),
            "error text must be descriptive; got {:?}",
            report.error
        );
    }
}
