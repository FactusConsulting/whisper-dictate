use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value};

use crate::config;

const WORKER_EVENT_PREFIX: &str = "[worker-event] ";
const HISTORY_KEYS: &[&str] = &[
    "ts",
    "event",
    "text",
    "raw_text",
    "text_preview",
    "text_chars",
    "dictionary_text",
    "recording_s",
    "capture_chunks_dropped",
    "pipeline_events_dropped",
    "pending_frames_dropped",
    "audio_duration_s",
    "compute_s",
    "real_time_factor",
    "language",
    "language_probability",
    "model",
    "stt_backend",
    "device",
    "compute_type",
    // Provenance (`crate::dictate::provenance`). Without these three the
    // DURABLE record is the only place that still cannot say which stack
    // served an utterance -- `stt_backend`/`device` above are the
    // configured values, and worker events + metrics are transient.
    // round 2. Mirrored in `vp_history._history_event`.
    "engine",
    "stt_impl",
    "stt_accel",
    "inject_mode",
    "inject_strategy",
    "target_title",
    "target_process",
    "profile",
    "dictionary_replacements",
    "dictionary_terms",
    "post_processor",
    "post_mode",
    "post_model",
    "post_latency_ms",
    "post_changed",
    "post_fallback",
    "post_error",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlPreview {
    pub path: PathBuf,
    pub total_rows: usize,
    pub shown_rows: usize,
    pub text: String,
}

pub fn preview_jsonl(path: impl Into<PathBuf>, limit: usize) -> Result<JsonlPreview> {
    let path = path.into();
    let mut total_rows = 0;
    let mut tail = crate::jsonl::Tail::new(limit);
    crate::jsonl::scan(&path, |row, bytes| {
        total_rows += 1;
        tail.push(row, bytes);
    })?;
    let shown = tail.into_rows();
    let text = shown.iter().map(format_row).collect::<Vec<_>>().join("\n");
    Ok(JsonlPreview {
        path,
        total_rows,
        shown_rows: shown.len(),
        text,
    })
}

pub fn handle_append_jsonl(path: &Path) -> Result<()> {
    let event = read_stdin_json()?;
    append_jsonl(path, &event)
}

pub fn handle_append_history(path: &Path) -> Result<()> {
    let event = read_stdin_json()?;
    let (settings, metrics_path) =
        crate::dictate::session::history_sink::effective_history_settings_with_metrics_path();
    if let Some(err) = settings.config_error {
        anyhow::bail!("history config read failed: {err}");
    }
    append_history_jsonl(path, &event, settings.max_entries, metrics_path.as_deref())
}

pub fn handle_append_record_sinks() -> Result<()> {
    let payload = read_stdin_json()?;
    append_record_sinks_payload(&payload)
}

pub fn append_record_sinks_payload(payload: &Value) -> Result<()> {
    let Some(event) = payload.get("event") else {
        return Ok(());
    };
    if let Some(path) = payload
        .get("metrics_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        append_jsonl(Path::new(path), event)?;
    }
    if let Some(path) = payload
        .get("history_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let max_entries = payload
            .get("history_max_entries")
            .map(|value| match value {
                Value::String(value) => crate::history_retention::parse_limit(value),
                other => crate::history_retention::parse_limit(&other.to_string()),
            })
            .unwrap_or(0);
        let metrics_path = payload
            .get("metrics_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(Path::new);
        append_history_jsonl(Path::new(path), event, max_entries, metrics_path)?;
    }
    Ok(())
}

pub fn handle_worker_event() -> Result<()> {
    let event = read_stdin_json()?;
    eprintln!("{}{}", WORKER_EVENT_PREFIX, serde_json::to_string(&event)?);
    Ok(())
}

pub fn append_jsonl(path: &Path, event: &Value) -> Result<()> {
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    let locked = crate::jsonl_file::acquire(path)?;
    crate::jsonl_file::append_locked(&locked.path, line.as_bytes())?;
    Ok(())
}

pub fn append_history_jsonl(
    path: &Path,
    event: &Value,
    max_entries: usize,
    metrics_path: Option<&Path>,
) -> Result<()> {
    crate::history_retention::append(path, &history_event(event), max_entries, metrics_path)?;
    Ok(())
}

pub fn history_event(event: &Value) -> Value {
    let Some(object) = event.as_object() else {
        return Value::Object(Map::new());
    };
    let mut filtered = Map::new();
    for key in HISTORY_KEYS {
        if let Some(value) = object.get(*key) {
            filtered.insert((*key).to_owned(), value.clone());
        }
    }
    Value::Object(filtered)
}

/// Resolve the on-disk history JSONL path from the user's settings, falling
/// back to the platform default when unset. Public because both the legacy
/// `history list [N]` verb (via [`preview_jsonl`]) and the new
/// `crate::history` CLI verbs need the same resolution.
pub fn history_path_from_settings() -> Result<PathBuf> {
    let settings = config::load_settings()?;
    Ok(history_path_for(&settings))
}

/// Resolve the history path from an already-loaded runtime snapshot.
pub fn history_path_for(settings: &config::AppSettings) -> PathBuf {
    if settings.history_jsonl.trim().is_empty() {
        config::default_history_path()
    } else {
        crate::dictate::expand_user(settings.history_jsonl.trim())
    }
}

fn read_stdin_json() -> Result<Value> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

fn format_row(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return value.to_string();
    };
    let mut parts = Vec::new();
    for key in [
        "text",
        "text_preview",
        "stt_backend",
        "model",
        "compute_s",
        "real_time_factor",
        "target_title",
        "post_processor",
        "post_error",
    ] {
        if let Some(value) = object.get(key) {
            if let Some(text) = value.as_str() {
                if !text.is_empty() {
                    parts.push(format!("{key}={text}"));
                }
            } else if !value.is_null() {
                parts.push(format!("{key}={value}"));
            }
        }
    }
    if parts.is_empty() {
        serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
    } else {
        parts.join("  ")
    }
}

// Read-only history queries live in `crate::history`; append helpers share
// one sidecar lock with opt-in retention so app-managed writers cannot lose
// a concurrent row during replacement.

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod bounded_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn history_path_for_uses_loaded_snapshot() {
        let settings = config::AppSettings {
            history_jsonl: "C:/saved/history.jsonl".to_owned(),
            ..config::AppSettings::default()
        };
        assert_eq!(
            history_path_for(&settings),
            PathBuf::from("C:/saved/history.jsonl")
        );
    }

    #[test]
    fn history_path_for_uses_default_when_snapshot_path_is_blank() {
        let settings = config::AppSettings {
            history_jsonl: "  ".to_owned(),
            ..config::AppSettings::default()
        };
        assert_eq!(history_path_for(&settings), config::default_history_path());
    }

    #[test]
    fn history_path_for_trims_and_expands_user_path_like_history_writer() {
        let settings = config::AppSettings {
            history_jsonl: "  ~/wd/history.jsonl  ".to_owned(),
            ..config::AppSettings::default()
        };
        assert_eq!(
            history_path_for(&settings),
            crate::dictate::expand_user("~/wd/history.jsonl")
        );
    }

    #[test]
    fn jsonl_preview_tails_and_formats_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        fs::write(
            &path,
            "{\"text\":\"first\",\"stt_backend\":\"whisper\"}\nnot json\n{\"text\":\"second\",\"model\":\"large-v3\"}\n",
        )
        .unwrap();

        let preview = preview_jsonl(&path, 1).unwrap();

        assert_eq!(preview.total_rows, 2);
        assert_eq!(preview.shown_rows, 1);
        assert!(preview.text.contains("text=second"));
        assert!(!preview.text.contains("first"));
    }

    // Moved: read_jsonl_rows coverage now lives in `crate::history::tests`
    // (`read_rows_missing_file_is_empty_no_error`,
    // `rows_from_str_skips_blank_and_malformed_lines`).

    #[test]
    fn append_jsonl_writes_utf8_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.jsonl");
        let event = serde_json::json!({"text": "rødgrød", "n": 1});

        append_jsonl(&path, &event).unwrap();

        let raw = fs::read_to_string(path).unwrap();
        assert_eq!(raw, "{\"n\":1,\"text\":\"rødgrød\"}\n");
    }

    #[test]
    fn append_jsonl_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("metrics.jsonl");

        append_jsonl(&path, &serde_json::json!({"event": "ok"})).unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "{\"event\":\"ok\"}\n");
    }

    #[test]
    fn append_record_sinks_payload_writes_metrics_and_filtered_history() {
        let dir = tempfile::tempdir().unwrap();
        let metrics = dir.path().join("metrics.jsonl");
        let history = dir.path().join("history.jsonl");
        let payload = serde_json::json!({
            "metrics_path": format!("  {}  ", metrics.display()),
            "history_path": format!("  {}  ", history.display()),
            "event": {
                "event": "utterance",
                "text": "hello",
                "api_key": "secret"
            }
        });

        append_record_sinks_payload(&payload).unwrap();

        let metrics_raw = fs::read_to_string(metrics).unwrap();
        let history_raw = fs::read_to_string(history).unwrap();
        assert!(metrics_raw.contains("\"api_key\":\"secret\""));
        assert!(history_raw.contains("\"text\":\"hello\""));
        assert!(!history_raw.contains("api_key"));
    }

    #[test]
    fn append_record_sinks_payload_ignores_whitespace_only_paths() {
        let payload = serde_json::json!({
            "metrics_path": "   ",
            "history_path": "\t",
            "event": {"text": "hello"}
        });

        append_record_sinks_payload(&payload).unwrap();
    }

    #[test]
    fn append_record_sinks_payload_noops_without_event() {
        let dir = tempfile::tempdir().unwrap();
        let metrics = dir.path().join("metrics.jsonl");
        let history = dir.path().join("history.jsonl");
        let payload = serde_json::json!({
            "metrics_path": metrics.display().to_string(),
            "history_path": history.display().to_string()
        });

        append_record_sinks_payload(&payload).unwrap();

        assert!(!metrics.exists());
        assert!(!history.exists());
    }

    #[test]
    fn history_event_keeps_only_core_fields() {
        let event = serde_json::json!({
            "ts": 1,
            "event": "utterance",
            "text": "hello",
            "target_title": "Editor",
            "large_unused_blob": "drop"
        });

        let filtered = history_event(&event);

        assert_eq!(filtered["text"], "hello");
        assert_eq!(filtered["target_title"], "Editor");
        assert!(filtered.get("large_unused_blob").is_none());
    }

    #[test]
    fn history_event_keeps_postprocess_fields_and_dictionary_data() {
        let event = serde_json::json!({
            "text": "clean text",
            "dictionary_replacements": [{"from": "lead death", "to": "lead dev"}],
            "dictionary_terms": ["Factus", "Codex"],
            "post_processor": "openai",
            "post_error": "rate limited",
            "api_key": "secret"
        });

        let filtered = history_event(&event);

        assert_eq!(filtered["post_processor"], "openai");
        assert_eq!(filtered["post_error"], "rate limited");
        assert_eq!(filtered["dictionary_replacements"][0]["to"], "lead dev");
        assert_eq!(
            filtered["dictionary_terms"],
            serde_json::json!(["Factus", "Codex"])
        );
        assert!(filtered.get("api_key").is_none());
    }

    /// history.jsonl is the DURABLE per-utterance record. Dropping the
    /// provenance fields here would leave it as the one place that still
    /// cannot say which stack served an utterance, while the transient
    /// worker events and metrics rows can. round 2.
    #[test]
    fn history_event_keeps_engine_impl_and_accel_provenance() {
        let event = serde_json::json!({
            "text": "hello",
            "engine": "rust-in-process",
            "stt_impl": "whisper.cpp",
            "stt_accel": "cpu",
            // The configured labels are kept too, and on their own they
            // cannot answer the same question -- that is why all six exist.
            "stt_backend": "whisper",
            "device": "auto",
        });

        let filtered = history_event(&event);

        assert_eq!(filtered["engine"], "rust-in-process");
        assert_eq!(filtered["stt_impl"], "whisper.cpp");
        assert_eq!(filtered["stt_accel"], "cpu");
        assert_eq!(filtered["stt_backend"], "whisper");
        assert_eq!(filtered["device"], "auto");
    }

    /// The written history LINE must carry them too -- the allowlist is
    /// applied inside `append_record_sinks_payload`, so asserting only on
    /// `history_event` would miss a sink that filtered twice.
    #[test]
    fn history_sink_writes_the_provenance_fields_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("history.jsonl");
        let payload = serde_json::json!({
            "event": {
                "event": "utterance",
                "text": "hello",
                "engine": "rust-in-process",
                "stt_impl": "whisper.cpp",
                "stt_accel": "vulkan",
            },
            "history_enabled": true,
            "history_path": history.display().to_string(),
        });

        append_record_sinks_payload(&payload).unwrap();

        let line = std::fs::read_to_string(&history).unwrap();
        let row: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(row["engine"], "rust-in-process");
        assert_eq!(row["stt_impl"], "whisper.cpp");
        assert_eq!(row["stt_accel"], "vulkan");
    }

    #[test]
    fn history_event_non_object_becomes_empty_object() {
        assert_eq!(
            history_event(&serde_json::json!("not an object")),
            serde_json::json!({})
        );
    }
}
