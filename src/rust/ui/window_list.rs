//! Parsing for the worker's `--list-windows` JSON output.
//!
//! The worker prints a JSON array of visible top-level windows on stdout, but
//! that stdout may carry leading log lines (version banner, config notes). The
//! parser is deliberately kept pure and small — it carves out the first `[` ..
//! last `]` span, parses it, and returns the `(title, process)` pairs the
//! Profiles tab shows. Unit-tested with and without surrounding log noise.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(in crate::ui) struct WindowEntry {
    pub(in crate::ui) title: String,
    #[serde(default)]
    pub(in crate::ui) process: String,
}

/// Parse the worker's `--list-windows` stdout into `(title, process)` pairs.
///
/// Returns `None` when the output cannot be parsed or contains no JSON array.
/// Tolerates surrounding log noise by extracting the first `[` .. last `]` span
/// (same heuristic as `audio_devices.rs`).
pub(in crate::ui) fn parse_windows_json(stdout: &str) -> Result<Vec<WindowEntry>, String> {
    let span = extract_json_array(stdout).ok_or_else(|| {
        if let Some(msg) = extract_error_message(stdout) {
            msg
        } else {
            "no window list found in worker output".to_owned()
        }
    })?;
    let entries: Vec<WindowEntry> =
        serde_json::from_str(span).map_err(|err| format!("could not parse window list: {err}"))?;
    Ok(entries)
}

/// Extract a JSON array span from stdout that may carry surrounding log noise.
/// Same algorithm as `audio_devices::extract_json_array`.
fn extract_json_array(stdout: &str) -> Option<&str> {
    let end = stdout.rfind(']')?;
    let mut search = 0;
    while let Some(rel) = stdout[search..=end].find('[') {
        let start = search + rel;
        let next = stdout[start + 1..=end]
            .bytes()
            .find(|b| !b.is_ascii_whitespace());
        let looks_like_array = match next {
            None => false,
            Some(b) => {
                matches!(b, b'{' | b'[' | b'"' | b']' | b'-' | b't' | b'f' | b'n')
                    || b.is_ascii_digit()
            }
        };
        if looks_like_array {
            return Some(&stdout[start..=end]);
        }
        if start >= end {
            break;
        }
        search = start + 1;
    }
    None
}

/// Pull the `error` field out of a `{"error": "..."}` object when present.
fn extract_error_message(stdout: &str) -> Option<String> {
    let start = stdout.find('{')?;
    let end = stdout.rfind('}')?;
    if end < start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&stdout[start..=end]).ok()?;
    value
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_json_array() {
        let stdout = r#"[{"title":"Notepad","process":"notepad.exe"},
            {"title":"Visual Studio Code","process":"Code.exe"}]"#;
        let entries = parse_windows_json(stdout).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "Notepad");
        assert_eq!(entries[0].process, "notepad.exe");
        assert_eq!(entries[1].title, "Visual Studio Code");
    }

    #[test]
    fn tolerates_surrounding_log_noise() {
        let stdout = "whisper-dictate 1.8.5\n[config] loaded\n\
            [{\"title\":\"Chrome\",\"process\":\"chrome.exe\"}]\n\
            trailing log\n";
        let entries = parse_windows_json(stdout).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Chrome");
        assert_eq!(entries[0].process, "chrome.exe");
    }

    #[test]
    fn skips_bracketed_log_tags_before_the_array() {
        let stdout = "[cap] probing\n[config] ok\n\
            [{\"title\":\"Slack\",\"process\":\"slack.exe\"}]";
        let entries = parse_windows_json(stdout).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Slack");
    }

    #[test]
    fn empty_array_yields_no_entries() {
        let entries = parse_windows_json("noise\n[]\nmore").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn reports_worker_error_object() {
        let stdout =
            "whisper-dictate 1.8.5\n{\"error\": \"window listing is only supported on Windows\"}\n";
        let err = parse_windows_json(stdout).unwrap_err();
        assert!(
            err.contains("window listing is only supported on Windows"),
            "{err}"
        );
    }

    #[test]
    fn reports_missing_array_when_no_json_present() {
        let err = parse_windows_json("just some logs, no json here\n").unwrap_err();
        assert!(err.contains("no window list found"), "{err}");
    }

    #[test]
    fn reports_malformed_array() {
        let err = parse_windows_json("[ not valid json ]").unwrap_err();
        assert!(err.contains("could not parse window list"), "{err}");
    }

    #[test]
    fn process_field_defaults_to_empty_string_when_absent() {
        let stdout = r#"[{"title":"Unknown App"}]"#;
        let entries = parse_windows_json(stdout).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Unknown App");
        assert_eq!(entries[0].process, "");
    }
}
