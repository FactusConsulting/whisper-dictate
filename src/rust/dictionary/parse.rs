//! Parsers for the on-disk dictionary file. Two shapes are supported:
//!
//! * **JSON** (the format the desktop UI writes) — an object with `terms`
//!   (string list, optionally `{"term": "..."}` objects) and `replacements`
//!   (either a `{from: to}` map or a list of `{from, to}` objects).
//! * **Plain text** (legacy + bring-your-own) — `terms:` / `replacements:`
//!   sections, one entry per line, with `=>` / `->` / `=` / `:` separators.
//!
//! The choice between the two is made by sniffing whether the first non-blank
//! character is `{` — if so we route to JSON, otherwise to the text parser.

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::{dedupe_terms, Dictionary, Replacement};

/// Parse a JSON dictionary document (string) into a [`Dictionary`]. The root
/// must be a JSON object; `terms` and `replacements` are both optional.
pub fn parse_json_dictionary(raw: &str) -> Result<Dictionary> {
    let value: Value = serde_json::from_str(raw)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("dictionary JSON root must be an object"))?;

    let terms = parse_dictionary_terms(object);
    let replacements = object
        .get("replacements")
        .map(parse_dictionary_replacements)
        .unwrap_or_default();

    Ok(Dictionary {
        terms: dedupe_terms(terms),
        replacements,
    })
}

/// Parse either shape: tries JSON when the first non-blank char is `{`,
/// otherwise falls back to the plain-text parser. Mirrors the Python loader.
pub fn parse_dictionary(raw: &str) -> Result<Dictionary> {
    if raw.trim_start().starts_with('{') {
        parse_json_dictionary(raw)
    } else {
        Ok(parse_text_dictionary(raw))
    }
}

/// Parse the plain-text dictionary shape (`terms:` / `replacements:` sections,
/// `-` bullets allowed, `=>` / `->` / `=` / `:` mapping separators, `#`
/// comments, blank lines ignored).
pub fn parse_text_dictionary(raw: &str) -> Dictionary {
    let mut terms = Vec::new();
    let mut replacements = Vec::new();
    let mut section = Section::Terms;

    for raw_line in raw.lines() {
        let mut line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let header = line.trim_end_matches(':').trim().to_ascii_lowercase();
        match header.as_str() {
            "[terms]" | "terms" => {
                section = Section::Terms;
                continue;
            }
            "[replacements]" | "replacements" => {
                section = Section::Replacements;
                continue;
            }
            _ => {}
        }
        if let Some(stripped) = line.strip_prefix('-') {
            line = stripped.trim();
        }
        match section {
            Section::Terms => terms.push(strip_quotes(line).to_owned()),
            Section::Replacements => {
                if let Some((from, to)) = parse_mapping_line(line) {
                    replacements.push(Replacement { from, to });
                }
            }
        }
    }

    Dictionary {
        terms: dedupe_terms(terms),
        replacements,
    }
}

pub(crate) fn parse_dictionary_terms(object: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut terms = Vec::new();
    let Some(raw_terms) = object.get("terms").and_then(Value::as_array) else {
        return terms;
    };
    for item in raw_terms {
        if let Some(term) = item.as_str() {
            terms.push(term.to_owned());
        } else if let Some(term) = item
            .as_object()
            .and_then(|object| object.get("term"))
            .and_then(Value::as_str)
        {
            terms.push(term.to_owned());
        }
    }
    terms
}

pub(crate) fn parse_dictionary_replacements(raw_replacements: &Value) -> Vec<Replacement> {
    let mut replacements = Vec::new();
    if let Some(map) = raw_replacements.as_object() {
        for (from, to) in map {
            replacements.push(Replacement {
                from: from.to_owned(),
                to: value_to_string(to),
            });
        }
    } else if let Some(items) = raw_replacements.as_array() {
        for item in items {
            let Some(object) = item.as_object() else {
                continue;
            };
            let Some(from) = object.get("from").and_then(Value::as_str) else {
                continue;
            };
            let Some(to) = object.get("to").and_then(Value::as_str) else {
                continue;
            };
            if from.is_empty() || to.is_empty() {
                continue;
            }
            replacements.push(Replacement {
                from: from.to_owned(),
                to: to.to_owned(),
            });
        }
    }
    replacements
}

pub(crate) fn parse_mapping_line(line: &str) -> Option<(String, String)> {
    for separator in ["=>", "->", "=", ":"] {
        if let Some((left, right)) = line.split_once(separator) {
            let left = strip_quotes(left.trim());
            let right = strip_quotes(right.trim());
            if !left.is_empty() && !right.is_empty() {
                return Some((left.to_owned(), right.to_owned()));
            }
        }
    }
    None
}

fn strip_quotes(value: &str) -> &str {
    value.trim_matches(|character| character == '"' || character == '\'')
}

fn value_to_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Terms,
    Replacements,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_dictionary_loads_terms_and_replacements() {
        let dictionary = parse_json_dictionary(
            r#"{
                "terms": ["Slack", {"term": "Claude Code"}, "", "slack"],
                "replacements": {"Cloud Code": "Claude Code", "code X": "Codex"}
            }"#,
        )
        .unwrap();

        assert_eq!(dictionary.terms, vec!["Slack", "Claude Code"]);
        assert_eq!(
            dictionary.replacements,
            vec![
                Replacement {
                    from: "Cloud Code".to_owned(),
                    to: "Claude Code".to_owned()
                },
                Replacement {
                    from: "code X".to_owned(),
                    to: "Codex".to_owned()
                }
            ]
        );
    }

    #[test]
    fn json_dictionary_accepts_replacement_list_shape() {
        let dictionary = parse_json_dictionary(
            r#"{
                "terms": [],
                "replacements": [
                    {"from": "lead death", "to": "lead dev"},
                    {"from": "", "to": "ignored"},
                    {"from": "missing"}
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            dictionary.replacements,
            vec![Replacement {
                from: "lead death".to_owned(),
                to: "lead dev".to_owned()
            }]
        );
    }

    #[test]
    fn text_dictionary_supports_sections_and_mapping_separators() {
        let dictionary = parse_text_dictionary(
            "terms:\n- OpenClaw\n- GitHub Actions\n\nreplacements:\nopen claw => OpenClaw\ncode X: Codex\n",
        );

        assert_eq!(dictionary.terms, vec!["OpenClaw", "GitHub Actions"]);
        assert_eq!(
            dictionary.replacements,
            vec![
                Replacement {
                    from: "open claw".to_owned(),
                    to: "OpenClaw".to_owned()
                },
                Replacement {
                    from: "code X".to_owned(),
                    to: "Codex".to_owned()
                }
            ]
        );
    }

    #[test]
    fn parse_dictionary_selects_json_or_text_shape() {
        let json =
            parse_dictionary(r#"{"terms":["Codex"],"replacements":{"code X":"Codex"}}"#).unwrap();
        let text = parse_dictionary("terms:\n- Codex\nreplacements:\ncode X => Codex\n").unwrap();

        assert_eq!(json, text);
    }
}
