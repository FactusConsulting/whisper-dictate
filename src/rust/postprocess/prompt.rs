//! Pure-string helpers: mode normalisation, prompt construction, and the
//! "extract the final rewrite out of a before/becomes/after answer" parser.
//!
//! Kept in a separate file so each helper is unit-tested without spinning up
//! HTTP servers and so the rest of [`crate::postprocess`] stays under the
//! 500-LOC ceiling.

use regex::{Regex, RegexBuilder};
use std::sync::OnceLock;

/// Normalise mode aliases. `bullet-list`, `bullet_list`, `bulletlist` all
/// fold to `bullets`; unknown values are lowercased and trimmed but not
/// further translated (so the validator can reject them downstream).
pub fn normalize_mode(mode: &str) -> String {
    let value = mode.trim().to_ascii_lowercase();
    if value.is_empty() {
        return "raw".to_owned();
    }
    match value.as_str() {
        "bullet-list" | "bullet_list" | "bulletlist" => "bullets".to_owned(),
        _ => value,
    }
}

/// Build the prompt sent to the LLM. Identical mode → instruction mapping as
/// the Python `build_prompt` so the cloud responses stay byte-equivalent.
pub fn build_prompt(text: &str, mode: &str) -> String {
    let mode = normalize_mode(mode);
    let instruction = match mode.as_str() {
        "prompt" => "Rewrite into a clear, actionable prompt for an AI coding agent. Preserve constraints, technical terms and intent. Do not add facts.",
        "terminal" => "Clean only obvious transcription artifacts. Preserve commands, flags, file paths, URLs, package names, product names, casing and code identifiers.",
        "slack" => "Rewrite as a concise Slack-style message. Keep it natural and faithful.",
        "email" => "Rewrite as a polished but faithful email. Preserve all concrete details.",
        "bullets" => "Rewrite as concise bullet points. Preserve all concrete details.",
        // "clean" and everything else fall back to the clean instructions.
        _ => "Clean punctuation, casing and only obvious transcription artifacts. Preserve the speaker's wording, word order and sentence structure unless grammar is clearly broken. Do not paraphrase or add facts.",
    };
    format!(
        "You are a local text post-processor for speech dictation.\nTask: {instruction}\nReturn only the rewritten text. If the input is already good, return it unchanged.\n\nDo not include the original text, labels, explanations, before/after formatting, or words such as 'becomes'.\n\nInput:\n{text}"
    )
}

fn final_marker_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        RegexBuilder::new(
            r"(?m)^\s*(?:becomes|bliver til|rewritten|rewrite|output|final|result|cleaned|rettet|endelig(?:\s+tekst)?)\s*:?\s*$",
        )
        .case_insensitive(true)
        .build()
        .expect("final marker regex must compile")
    })
}

fn inline_final_marker_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        RegexBuilder::new(r"\s+(?:becomes|bliver til|=>|->|→)\s+")
            .case_insensitive(true)
            .build()
            .expect("inline final marker regex must compile")
    })
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim().to_lowercase()
}

/// Pull the "final" rewrite out of a model response that echoed the original
/// text in a `before / becomes / after` shape (a common Danish-prompted
/// regression — see Python `_extract_final_text`).
pub fn extract_final_text(output: &str, source_text: &str) -> String {
    let out = output.trim();
    let source = source_text.trim();
    if out.is_empty() || source.is_empty() {
        return out.to_owned();
    }
    let source_cmp = collapse_whitespace(source);

    for marker in final_marker_regex().find_iter(out) {
        let prefix = &out[..marker.start()];
        let final_part = out[marker.end()..].trim();
        if !final_part.is_empty() && collapse_whitespace(prefix).contains(&source_cmp) {
            return final_part.to_owned();
        }
    }

    for marker in inline_final_marker_regex().find_iter(out) {
        let prefix = &out[..marker.start()];
        let final_part = out[marker.end()..].trim();
        if !final_part.is_empty() && collapse_whitespace(prefix) == source_cmp {
            return final_part.to_owned();
        }
    }

    out.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_mode_handles_aliases_and_empty() {
        assert_eq!(normalize_mode("BULLET-LIST"), "bullets");
        assert_eq!(normalize_mode(" bullet_list "), "bullets");
        assert_eq!(normalize_mode("bulletlist"), "bullets");
        assert_eq!(normalize_mode("Clean"), "clean");
        assert_eq!(normalize_mode(""), "raw");
        assert_eq!(normalize_mode("   "), "raw");
    }

    #[test]
    fn build_prompt_covers_every_roadmap_mode() {
        let expectations: &[(&str, &str)] = &[
            ("clean", "Clean punctuation"),
            ("prompt", "AI coding agent"),
            ("terminal", "Preserve commands"),
            ("slack", "Slack-style message"),
            ("email", "polished but faithful email"),
            ("bullets", "concise bullet points"),
            ("bullet-list", "concise bullet points"),
        ];
        for (mode, phrase) in expectations {
            let prompt = build_prompt("hello world", mode);
            assert!(prompt.contains(phrase), "{mode} missing {phrase}");
            assert!(prompt.contains("Return only the rewritten text"));
            assert!(prompt.contains("Do not include the original text"));
        }
        assert!(build_prompt("x", "clean").contains("Do not paraphrase"));
    }

    #[test]
    fn extract_final_text_pulls_after_becomes_marker() {
        let source = "Hej, mit navn er Sara. Jeg er Lars' datter.";
        let final_part = "Hej, mit navn er Sara. Jeg er datter af Lars.";
        let output = format!("{source}\n\nbecomes\n\n{final_part}");

        assert_eq!(extract_final_text(&output, source), final_part);
    }

    #[test]
    fn extract_final_text_keeps_output_when_no_marker_matches() {
        let output = "Just cleaned text without any markers.";
        assert_eq!(extract_final_text(output, "source"), output);
    }

    #[test]
    fn extract_final_text_handles_inline_arrow_marker() {
        let result = extract_final_text("hello world => Hello, world.", "hello world");
        assert_eq!(result, "Hello, world.");
    }

    #[test]
    fn extract_final_text_returns_empty_when_inputs_are_empty() {
        assert_eq!(extract_final_text("", "source"), "");
        assert_eq!(extract_final_text("output", ""), "output");
    }
}
