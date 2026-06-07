use std::io::{self, Read};

use anyhow::Result;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Redaction {
    pub placeholder: String,
    pub value: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RedactionResult {
    pub text: String,
    pub redactions: Vec<Redaction>,
}

#[derive(Debug, Deserialize)]
struct RedactRequest {
    text: String,
    #[serde(default)]
    terms: Vec<String>,
}

pub fn handle_redact_text() -> Result<()> {
    let request = read_request()?;
    let result = redact_text(&request.text, &request.terms);
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

pub fn redact_text(text: &str, terms: &[String]) -> RedactionResult {
    let mut redactions = Vec::new();
    let mut out = text.to_owned();
    out = replace_regex(&out, email_regex(), "email", &mut redactions);
    out = replace_regex(&out, token_regex(), "token", &mut redactions);
    out = replace_phone(&out, &mut redactions);
    if let Some(regex) = term_regex(terms) {
        out = replace_regex(&out, regex, "term", &mut redactions);
    }
    RedactionResult {
        text: out,
        redactions,
    }
}

fn replace_regex(
    text: &str,
    regex: regex::Regex,
    kind: &str,
    redactions: &mut Vec<Redaction>,
) -> String {
    regex
        .replace_all(text, |captures: &regex::Captures<'_>| {
            let value = captures.get(0).map(|m| m.as_str()).unwrap_or_default();
            let placeholder = format!(
                "[[WD_{}_{}]]",
                kind.to_ascii_uppercase(),
                redactions.len() + 1
            );
            redactions.push(Redaction {
                placeholder: placeholder.clone(),
                value: value.to_owned(),
                kind: kind.to_owned(),
            });
            placeholder
        })
        .into_owned()
}

fn replace_phone(text: &str, redactions: &mut Vec<Redaction>) -> String {
    phone_regex()
        .replace_all(text, |captures: &regex::Captures<'_>| {
            let prefix = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
            let value = captures.get(2).map(|m| m.as_str()).unwrap_or_default();
            let suffix = captures.get(3).map(|m| m.as_str()).unwrap_or_default();
            let placeholder = format!("[[WD_PHONE_{}]]", redactions.len() + 1);
            redactions.push(Redaction {
                placeholder: placeholder.clone(),
                value: value.to_owned(),
                kind: "phone".to_owned(),
            });
            format!("{prefix}{placeholder}{suffix}")
        })
        .into_owned()
}

fn email_regex() -> regex::Regex {
    RegexBuilder::new(r"\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b")
        .case_insensitive(true)
        .build()
        .expect("email regex must compile")
}

fn token_regex() -> regex::Regex {
    regex::Regex::new(
        r"\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9_]{16,}|xox[baprs]-[A-Za-z0-9-]{16,})\b",
    )
    .expect("token regex must compile")
}

fn phone_regex() -> regex::Regex {
    regex::Regex::new(
        r"(^|[^\p{Alphabetic}\p{Number}_])(\+?\d[\d .()/-]{6,}\d)($|[^\p{Alphabetic}\p{Number}_])",
    )
    .expect("phone regex must compile")
}

fn term_regex(terms: &[String]) -> Option<regex::Regex> {
    let mut cleaned = terms
        .iter()
        .map(|term| term.trim())
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if cleaned.is_empty() {
        return None;
    }
    cleaned.sort_by_key(|term| std::cmp::Reverse(term.chars().count()));
    let pattern = format!(
        r"\b({})\b",
        cleaned
            .iter()
            .map(|term| regex::escape(term))
            .collect::<Vec<_>>()
            .join("|")
    );
    Some(
        RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .expect("term regex must compile"),
    )
}

fn read_request() -> Result<RedactRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_email_token_phone_and_terms() {
        let result = redact_text(
            "Mail lars@example.com, call +45 12 34 56 78, token sk-abcdefghijklmnop, project Codex.",
            &[String::from("Codex")],
        );

        assert!(result.text.contains("[[WD_EMAIL_1]]"));
        assert!(result.text.contains("[[WD_TOKEN_2]]"));
        assert!(result.text.contains("[[WD_PHONE_3]]"));
        assert!(result.text.contains("[[WD_TERM_4]]"));
        assert_eq!(result.redactions[0].kind, "email");
        assert_eq!(result.redactions[3].value, "Codex");
    }

    #[test]
    fn term_redaction_prefers_longest_terms() {
        let result = redact_text(
            "Claude Code and Claude",
            &[String::from("Claude"), String::from("Claude Code")],
        );

        assert_eq!(result.redactions[0].value, "Claude Code");
        assert_eq!(result.redactions[1].value, "Claude");
    }

    #[test]
    fn term_redaction_is_case_insensitive_and_whole_word_only() {
        let result = redact_text(
            "codex is hidden but codec and mycodex stay",
            &[String::from("Codex")],
        );

        assert_eq!(result.redactions.len(), 1);
        assert_eq!(result.redactions[0].value, "codex");
        assert!(result.text.contains("codec"));
        assert!(result.text.contains("mycodex"));
    }

    #[test]
    fn phone_redaction_preserves_surrounding_punctuation() {
        let result = redact_text("Call (+45 12 34 56 78), thanks.", &[]);

        assert_eq!(result.redactions.len(), 1);
        assert_eq!(result.redactions[0].kind, "phone");
        assert_eq!(result.redactions[0].value, "+45 12 34 56 78");
        assert!(result.text.contains("([[WD_PHONE_1]]), thanks."));
    }

    #[test]
    fn tokens_are_redacted_without_public_value() {
        let result = redact_text("use ghp_abcdefghijklmnop1234 now", &[]);

        assert_eq!(result.redactions.len(), 1);
        assert_eq!(result.redactions[0].kind, "token");
        assert!(!result.text.contains("ghp_abcdefghijklmnop1234"));
    }
}
