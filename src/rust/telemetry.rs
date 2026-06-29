use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value};

use crate::cli::HistoryCommand;
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
    "audio_duration_s",
    "compute_s",
    "real_time_factor",
    "language",
    "language_probability",
    "model",
    "stt_backend",
    "device",
    "compute_type",
    "inject_mode",
    "inject_strategy",
    "target_title",
    "target_process",
    "profile",
    "dictionary_replacements",
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
    let raw = fs::read_to_string(&path)?;
    let mut rows = Vec::new();
    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            rows.push(value);
        }
    }
    let total_rows = rows.len();
    let limit = limit.max(1);
    let start = total_rows.saturating_sub(limit);
    let shown = &rows[start..];
    let text = shown.iter().map(format_row).collect::<Vec<_>>().join("\n");
    Ok(JsonlPreview {
        path,
        total_rows,
        shown_rows: shown.len(),
        text,
    })
}

pub fn handle_history_command(command: HistoryCommand) -> Result<()> {
    match command {
        HistoryCommand::List { limit } => {
            let path = history_path_from_settings()?;
            let preview = preview_jsonl(&path, limit)?;
            if !preview.text.is_empty() {
                println!("{}", preview.text);
            }
            Ok(())
        }
        HistoryCommand::Last => {
            let path = history_path_from_settings()?;
            if let Some(row) = read_jsonl_rows(&path)?.pop() {
                if let Some(text) = row.get("text").and_then(Value::as_str) {
                    println!("{text}");
                }
            }
            Ok(())
        }
        HistoryCommand::Search {
            query,
            limit,
            offset,
        } => handle_history_search(&query, limit, offset),
    }
}

/// SQLite-backed search subcommand for `whisper-dictate history search`.
///
/// Wired separately from the JSONL preview path used by `list` / `last`
/// so the existing flat-file workflow stays unchanged (issue #324
/// adds the SQLite store side-by-side with the JSONL sink rather
/// than replacing it).
#[cfg(feature = "history-sqlite")]
fn handle_history_search(query: &str, limit: u32, offset: u32) -> Result<()> {
    use crate::history::{open_default, search, SearchOptions};
    let conn = open_default()?;
    let query = query.trim();
    let hits = search(
        &conn,
        &SearchOptions {
            query: (!query.is_empty()).then(|| query.to_owned()),
            limit: Some(limit),
            offset,
            ..Default::default()
        },
    )?;
    for hit in hits {
        let mut parts = vec![format!("ts={}", hit.ts)];
        if let Some(backend) = hit.stt_backend.as_deref() {
            parts.push(format!("backend={backend}"));
        }
        if let Some(model) = hit.model.as_deref() {
            parts.push(format!("model={model}"));
        }
        if let Some(score) = hit.score {
            parts.push(format!("score={score:.3}"));
        }
        parts.push(format!("text={}", hit.text));
        println!("{}", parts.join("  "));
    }
    Ok(())
}

/// Stock-build stub for `whisper-dictate history search`. Users that
/// rebuild with `--no-default-features` lose the SQLite store; the
/// subcommand stays exposed so the CLI surface is feature-flag-stable
/// but reports the missing-feature situation rather than silently
/// returning no rows.
#[cfg(not(feature = "history-sqlite"))]
fn handle_history_search(_query: &str, _limit: u32, _offset: u32) -> Result<()> {
    Err(anyhow::anyhow!(
        "history search requires the `history-sqlite` feature \
         (default on; rebuild without --no-default-features to enable)"
    ))
}

pub fn handle_append_jsonl(path: &Path) -> Result<()> {
    let event = read_stdin_json()?;
    append_jsonl(path, &event)
}

pub fn handle_append_history(path: &Path) -> Result<()> {
    let event = read_stdin_json()?;
    append_jsonl(path, &history_event(&event))
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
        append_jsonl(Path::new(path), &history_event(event))?;
    }
    Ok(())
}

pub fn handle_worker_event() -> Result<()> {
    let event = read_stdin_json()?;
    eprintln!("{}{}", WORKER_EVENT_PREFIX, serde_json::to_string(&event)?);
    Ok(())
}

pub fn append_jsonl(path: &Path, event: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())?;
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

fn history_path_from_settings() -> Result<PathBuf> {
    let settings = config::load_settings()?;
    if settings.history_jsonl.trim().is_empty() {
        Ok(config::default_history_path())
    } else {
        Ok(PathBuf::from(settings.history_jsonl))
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

fn read_jsonl_rows(path: &PathBuf) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn read_jsonl_rows_ignores_invalid_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        fs::write(
            &path,
            "{\"text\":\"first\"}\nnot json\n{\"text\":\"last\"}\n",
        )
        .unwrap();

        let rows = read_jsonl_rows(&path).unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows.last().unwrap()["text"], "last");
    }

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
    fn history_event_keeps_postprocess_fields_and_replacements() {
        let event = serde_json::json!({
            "text": "clean text",
            "dictionary_replacements": [{"from": "lead death", "to": "lead dev"}],
            "post_processor": "openai",
            "post_error": "rate limited",
            "api_key": "secret"
        });

        let filtered = history_event(&event);

        assert_eq!(filtered["post_processor"], "openai");
        assert_eq!(filtered["post_error"], "rate limited");
        assert_eq!(filtered["dictionary_replacements"][0]["to"], "lead dev");
        assert!(filtered.get("api_key").is_none());
    }

    #[test]
    fn history_event_non_object_becomes_empty_object() {
        assert_eq!(
            history_event(&serde_json::json!("not an object")),
            serde_json::json!({})
        );
    }
}
