//! Shared helpers for parsing worker JSON output.
//!
//! Both `audio_devices` and `window_list` parse a JSON array emitted on the
//! worker's stdout (optionally surrounded by log noise) and surface a worker
//! `{"error": "..."}` object as an `Err` string.  The two algorithms are
//! identical, so they live here once and are used by both callers.

/// Extract a JSON array span (`[` .. last `]`) from a stdout blob that may
/// carry surrounding log noise.  Log lines often contain brackets (e.g.
/// `[config]`), so the opening bracket is the first `[` whose next
/// non-whitespace character actually starts a JSON value
/// (`{ [ " - digit t f n`) or closes an empty array (`]`); anything else
/// (a log tag) is skipped.  Returns `None` when no such array start pairs
/// with a trailing `]`.
pub(in crate::ui) fn extract_json_array(stdout: &str) -> Option<&str> {
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

/// Extract a JSON object span (`{` .. last `}`) from a stdout blob that may
/// carry surrounding log noise. Mirrors [`extract_json_array`] for the
/// single-object case (the `--test-audio-device` result): the opening brace is
/// the first `{` whose next non-whitespace character starts a JSON object key
/// (`"`) or closes an empty object (`}`); a stray `{` inside a log line is
/// skipped. Returns `None` when no such object start pairs with a trailing `}`.
pub(in crate::ui) fn extract_json_object(stdout: &str) -> Option<&str> {
    let end = stdout.rfind('}')?;
    let mut search = 0;
    while let Some(rel) = stdout[search..=end].find('{') {
        let start = search + rel;
        let next = stdout[start + 1..=end]
            .bytes()
            .find(|b| !b.is_ascii_whitespace());
        let looks_like_object = matches!(next, Some(b'"') | Some(b'}'));
        if looks_like_object {
            return Some(&stdout[start..=end]);
        }
        if start >= end {
            break;
        }
        search = start + 1;
    }
    None
}

/// Pull the `error` field out of a `{"error": "..."}` object, if present, so
/// the no-array path can report the worker's own message instead of a generic
/// fallback.
pub(in crate::ui) fn extract_error_message(stdout: &str) -> Option<String> {
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

    // --- extract_json_array ---

    #[test]
    fn finds_plain_array() {
        let s = r#"[{"a":1}]"#;
        assert_eq!(extract_json_array(s), Some(s));
    }

    #[test]
    fn skips_bracketed_log_tags() {
        let s = "[cap] probing\n[{\"a\":1}]";
        assert_eq!(extract_json_array(s), Some(r#"[{"a":1}]"#));
    }

    #[test]
    fn returns_none_when_no_array() {
        assert_eq!(extract_json_array("just text"), None);
    }

    #[test]
    fn empty_array_is_found() {
        assert_eq!(extract_json_array("noise\n[]\nmore"), Some("[]"));
    }

    // --- extract_json_object ---

    #[test]
    fn finds_plain_object() {
        let s = r#"{"a":1}"#;
        assert_eq!(extract_json_object(s), Some(s));
    }

    #[test]
    fn object_skips_bracketed_log_tags() {
        // A leading `[cap]` log tag has no `{`, so the object start is found.
        let s = "[cap] probing\n{\"usable\":true}";
        assert_eq!(extract_json_object(s), Some(r#"{"usable":true}"#));
    }

    #[test]
    fn empty_object_is_found() {
        assert_eq!(extract_json_object("noise\n{}\nmore"), Some("{}"));
    }

    #[test]
    fn object_returns_none_when_no_object() {
        assert_eq!(extract_json_object("just [text] here"), None);
    }

    // --- extract_error_message ---

    #[test]
    fn extracts_error_field() {
        let s = r#"{"error": "something went wrong"}"#;
        assert_eq!(
            extract_error_message(s),
            Some("something went wrong".to_owned())
        );
    }

    #[test]
    fn returns_none_when_no_object() {
        assert_eq!(extract_error_message("plain text"), None);
    }

    #[test]
    fn returns_none_when_object_has_no_error_key() {
        assert_eq!(extract_error_message(r#"{"ok": true}"#), None);
    }
}
