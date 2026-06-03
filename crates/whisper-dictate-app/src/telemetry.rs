use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

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
}
