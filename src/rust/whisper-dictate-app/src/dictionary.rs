use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use regex::RegexBuilder;
use serde_json::{Map, Value};

use crate::cli::DictionaryCommand;
use crate::config;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dictionary {
    pub terms: Vec<String>,
    pub replacements: Vec<Replacement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacementChange {
    pub from: String,
    pub to: String,
    pub count: usize,
}

impl Dictionary {
    pub fn prompt_terms(&self, max_terms: usize, max_chars: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = 0;
        for term in &self.terms {
            let added = term.chars().count() + if out.is_empty() { 0 } else { 2 };
            if out.len() >= max_terms || chars + added > max_chars {
                break;
            }
            out.push(term.clone());
            chars += added;
        }
        out
    }

    pub fn build_prompt(
        &self,
        base_prompt: Option<&str>,
        max_terms: usize,
        max_chars: usize,
    ) -> Option<String> {
        let terms = self.prompt_terms(max_terms, max_chars);
        let mut parts = Vec::new();
        if let Some(base_prompt) = base_prompt.map(str::trim).filter(|value| !value.is_empty()) {
            parts.push(base_prompt.to_owned());
        }
        if !terms.is_empty() {
            parts.push(format!("Vocabulary: {}", terms.join(", ")));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }

    pub fn apply_replacements(&self, text: &str) -> Result<(String, Vec<ReplacementChange>)> {
        if text.is_empty() || self.replacements.is_empty() {
            return Ok((text.to_owned(), Vec::new()));
        }

        let mut out = text.to_owned();
        let mut changes = Vec::new();
        let mut replacements = self.replacements.clone();
        replacements.sort_by(|a, b| b.from.chars().count().cmp(&a.from.chars().count()));

        for replacement in replacements {
            if replacement.from.is_empty() {
                continue;
            }
            let pattern = format!(
                r"(^|[^\p{{Alphabetic}}\p{{Number}}_])({})([^\p{{Alphabetic}}\p{{Number}}_]|$)",
                regex::escape(&replacement.from)
            );
            let regex = RegexBuilder::new(&pattern).case_insensitive(true).build()?;
            let mut count = 0;
            let rewritten = regex.replace_all(&out, |captures: &regex::Captures<'_>| {
                count += 1;
                format!("{}{}{}", &captures[1], replacement.to, &captures[3])
            });
            if count > 0 {
                out = rewritten.into_owned();
                changes.push(ReplacementChange {
                    from: replacement.from,
                    to: replacement.to,
                    count,
                });
            }
        }

        Ok((out, changes))
    }
}

pub fn parse_json_dictionary(raw: &str) -> Result<Dictionary> {
    let value: Value = serde_json::from_str(raw)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("dictionary JSON root must be an object"))?;

    let mut terms = Vec::new();
    if let Some(raw_terms) = object.get("terms").and_then(Value::as_array) {
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
    }

    let mut replacements = Vec::new();
    if let Some(raw_replacements) = object.get("replacements") {
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
    }

    Ok(Dictionary {
        terms: dedupe_terms(terms),
        replacements,
    })
}

pub fn parse_dictionary(raw: &str) -> Result<Dictionary> {
    if raw.trim_start().starts_with('{') {
        parse_json_dictionary(raw)
    } else {
        Ok(parse_text_dictionary(raw))
    }
}

pub fn load_dictionary(path: impl AsRef<Path>) -> Result<Dictionary> {
    let raw = std::fs::read_to_string(path)?;
    parse_dictionary(&raw)
}

pub fn handle_command(command: DictionaryCommand) -> Result<()> {
    let settings = config::load_settings()?;
    let path = PathBuf::from(&settings.dictionary);
    match command {
        DictionaryCommand::Status => {
            let preview = preview_dictionary(
                &path,
                Some(&settings.initial_prompt),
                settings.dictionary_max_terms.parse().unwrap_or(80),
                settings.dictionary_prompt_chars.parse().unwrap_or(1200),
            )?;
            println!("path: {}", preview.path.display());
            println!("terms: {}", preview.term_count);
            println!("replacements: {}", preview.replacement_count);
            if let Some(prompt) = preview.prompt {
                println!("prompt:\n{prompt}");
            }
        }
        DictionaryCommand::Open => {
            let path = config::open_dictionary(path)?;
            println!("opened: {}", path.display());
        }
        DictionaryCommand::Add { term } => {
            let added = add_term(&path, &term)?;
            println!(
                "{}: {}",
                if added { "added" } else { "already present" },
                path.display()
            );
        }
        DictionaryCommand::Replace { mapping } => {
            let (from, to, changed) = add_replacement(&path, &mapping)?;
            println!(
                "{}: {from} => {to} ({})",
                if changed { "saved" } else { "unchanged" },
                path.display()
            );
        }
    }
    Ok(())
}

pub fn ensure_json_dictionary(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if !path.exists() {
        path.parent().map(fs::create_dir_all).transpose()?;
        write_json_dictionary(path, &Dictionary::default(), None)?;
    }
    Ok(path.to_path_buf())
}

pub fn add_term(path: impl AsRef<Path>, term: &str) -> Result<bool> {
    let term = term.trim();
    if term.is_empty() {
        return Err(anyhow!("dictionary term cannot be empty"));
    }
    let path = path.as_ref();
    let (mut dictionary, base) = load_json_dictionary_for_write(path)?;
    let before = dictionary.terms.len();
    dictionary.terms = dedupe_terms(
        dictionary
            .terms
            .into_iter()
            .chain(std::iter::once(term.to_owned()))
            .collect(),
    );
    let added = dictionary.terms.len() != before;
    if added {
        write_json_dictionary(path, &dictionary, Some(base))?;
    }
    Ok(added)
}

pub fn add_replacement(path: impl AsRef<Path>, mapping: &str) -> Result<(String, String, bool)> {
    let (from, to) =
        parse_mapping_line(mapping).ok_or_else(|| anyhow!("replacement must be FROM=TO"))?;
    let path = path.as_ref();
    let (mut dictionary, base) = load_json_dictionary_for_write(path)?;
    let changed = dictionary
        .replacements
        .iter()
        .find(|replacement| replacement.from == from)
        .is_none_or(|replacement| replacement.to != to);
    if changed {
        dictionary
            .replacements
            .retain(|replacement| replacement.from != from);
        dictionary.replacements.push(Replacement {
            from: from.clone(),
            to: to.clone(),
        });
        write_json_dictionary(path, &dictionary, Some(base))?;
    }
    Ok((from, to, changed))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryPreview {
    pub path: PathBuf,
    pub term_count: usize,
    pub replacement_count: usize,
    pub prompt: Option<String>,
}

pub fn preview_dictionary(
    path: impl Into<PathBuf>,
    base_prompt: Option<&str>,
    max_terms: usize,
    max_chars: usize,
) -> Result<DictionaryPreview> {
    let path = path.into();
    let dictionary = load_dictionary(&path)?;
    Ok(DictionaryPreview {
        path,
        term_count: dictionary.terms.len(),
        replacement_count: dictionary.replacements.len(),
        prompt: dictionary.build_prompt(base_prompt, max_terms, max_chars),
    })
}

fn load_json_dictionary_for_write(path: &Path) -> Result<(Dictionary, Map<String, Value>)> {
    ensure_json_dictionary(path)?;
    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let base = value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("dictionary JSON root must be an object"))?;
    let dictionary = parse_json_dictionary(&raw)?;
    Ok((dictionary, base))
}

fn write_json_dictionary(
    path: &Path,
    dictionary: &Dictionary,
    base: Option<Map<String, Value>>,
) -> Result<()> {
    path.parent().map(fs::create_dir_all).transpose()?;
    let mut object = base.unwrap_or_default();
    object.insert(
        "terms".to_owned(),
        Value::Array(
            dedupe_terms(dictionary.terms.clone())
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
    );
    let mut replacements = Map::new();
    for replacement in sorted_replacements(&dictionary.replacements) {
        replacements.insert(
            replacement.from.clone(),
            Value::String(replacement.to.clone()),
        );
    }
    object.insert("replacements".to_owned(), Value::Object(replacements));
    fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(object))? + "\n",
    )?;
    Ok(())
}

fn sorted_replacements(replacements: &[Replacement]) -> Vec<Replacement> {
    let mut out = replacements.to_vec();
    out.sort_by(|a, b| a.from.cmp(&b.from));
    out
}

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

fn parse_mapping_line(line: &str) -> Option<(String, String)> {
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

fn dedupe_terms(terms: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for term in terms {
        let term = term.trim();
        let key = term.to_casefold();
        if !term.is_empty() && seen.insert(key) {
            out.push(term.to_owned());
        }
    }
    out
}

trait CaseFold {
    fn to_casefold(&self) -> String;
}

impl CaseFold for str {
    fn to_casefold(&self) -> String {
        self.to_lowercase()
    }
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

    #[test]
    fn preview_dictionary_reports_counts_and_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        std::fs::write(
            &path,
            r#"{"terms":["Codex","Claude Code"],"replacements":{"code X":"Codex"}}"#,
        )
        .unwrap();

        let preview = preview_dictionary(&path, Some("Base prompt"), 10, 1200).unwrap();

        assert_eq!(preview.path, path);
        assert_eq!(preview.term_count, 2);
        assert_eq!(preview.replacement_count, 1);
        assert_eq!(
            preview.prompt.as_deref(),
            Some("Base prompt\nVocabulary: Codex, Claude Code")
        );
    }

    #[test]
    fn add_term_creates_json_dictionary_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");

        assert!(add_term(&path, "Codex").unwrap());
        assert!(!add_term(&path, "codex").unwrap());

        let dictionary = load_dictionary(&path).unwrap();
        assert_eq!(dictionary.terms, vec!["Codex"]);
    }

    #[test]
    fn add_replacement_preserves_unknown_json_fields_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        std::fs::write(
            &path,
            r#"{"terms":["Codex"],"notes":"keep","replacements":{"z":"Z"}}"#,
        )
        .unwrap();

        let (from, to, changed) = add_replacement(&path, "a=A").unwrap();

        assert_eq!((from.as_str(), to.as_str(), changed), ("a", "A", true));
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["notes"], "keep");
        let keys = value["replacements"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["a", "z"]);
    }

    #[test]
    fn prompt_terms_respect_term_and_character_caps() {
        let dictionary = Dictionary {
            terms: vec![
                "Slack".to_owned(),
                "Claude Code".to_owned(),
                "Codex".to_owned(),
            ],
            replacements: Vec::new(),
        };

        assert_eq!(
            dictionary.prompt_terms(2, 1200),
            vec!["Slack", "Claude Code"]
        );
        assert_eq!(
            dictionary.prompt_terms(80, 18),
            vec!["Slack", "Claude Code"]
        );
        assert_eq!(dictionary.prompt_terms(80, 17), vec!["Slack"]);
    }

    #[test]
    fn build_prompt_appends_vocabulary_to_base_prompt() {
        let dictionary = Dictionary {
            terms: vec!["Slack".to_owned(), "Claude Code".to_owned()],
            replacements: Vec::new(),
        };

        assert_eq!(
            dictionary.build_prompt(Some("Base prompt"), 80, 1200),
            Some("Base prompt\nVocabulary: Slack, Claude Code".to_owned())
        );
    }

    #[test]
    fn replacements_are_case_insensitive_whole_word_and_sequential() {
        let dictionary = Dictionary {
            terms: Vec::new(),
            replacements: vec![
                Replacement {
                    from: "Code".to_owned(),
                    to: "Wrong".to_owned(),
                },
                Replacement {
                    from: "Cloud Code".to_owned(),
                    to: "Claude Code".to_owned(),
                },
                Replacement {
                    from: "code X".to_owned(),
                    to: "Codex".to_owned(),
                },
            ],
        };

        let (text, changes) = dictionary
            .apply_replacements("Open Cloud Code and code X. Cloud Codes stay.")
            .unwrap();

        assert_eq!(text, "Open Claude Wrong and Codex. Cloud Codes stay.");
        assert_eq!(
            changes,
            vec![
                ReplacementChange {
                    from: "Cloud Code".to_owned(),
                    to: "Claude Code".to_owned(),
                    count: 1,
                },
                ReplacementChange {
                    from: "code X".to_owned(),
                    to: "Codex".to_owned(),
                    count: 1,
                },
                ReplacementChange {
                    from: "Code".to_owned(),
                    to: "Wrong".to_owned(),
                    count: 1,
                }
            ]
        );
    }
}
