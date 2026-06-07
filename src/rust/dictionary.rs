use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cli::DictionaryCommand;
use crate::config;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dictionary {
    pub terms: Vec<String>,
    pub replacements: Vec<Replacement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Replacement {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplacementChange {
    pub from: String,
    pub to: String,
    pub count: usize,
}

#[derive(Debug, Deserialize)]
struct RuntimeRequest {
    #[serde(default)]
    base_prompt: Option<String>,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDictionarySettings {
    pub enabled: bool,
    pub paths: Vec<PathBuf>,
    pub max_terms: usize,
    pub max_chars: usize,
}

impl RuntimeDictionarySettings {
    pub fn new(enabled: bool, paths: Vec<PathBuf>, max_terms: usize, max_chars: usize) -> Self {
        Self {
            enabled,
            paths,
            max_terms,
            max_chars,
        }
    }

    fn from_env_and_config() -> Self {
        let configured = config::load_settings().unwrap_or_default();
        let enabled =
            env_bool("VOICEPI_DICTIONARY_ENABLED").unwrap_or(configured.dictionary_enabled);
        let paths = env_paths("VOICEPI_DICTIONARY").unwrap_or_else(|| {
            let path = configured.dictionary.trim();
            if path.is_empty() {
                Vec::new()
            } else {
                vec![PathBuf::from(path)]
            }
        });
        let max_terms = env_usize("VOICEPI_DICTIONARY_MAX_TERMS")
            .or_else(|| configured.dictionary_max_terms.parse::<usize>().ok())
            .unwrap_or(80);
        let max_chars = env_usize("VOICEPI_DICTIONARY_PROMPT_CHARS")
            .or_else(|| configured.dictionary_prompt_chars.parse::<usize>().ok())
            .unwrap_or(1200);

        Self::new(enabled, paths, max_terms, max_chars)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeDictionaryResult {
    pub enabled: bool,
    pub path: Option<String>,
    pub loaded_paths: Vec<String>,
    pub term_count: usize,
    pub replacement_count: usize,
    pub terms: Vec<String>,
    pub all_terms: Vec<String>,
    pub replacements: Vec<Replacement>,
    pub prompt: Option<String>,
    pub text: String,
    pub changes: Vec<ReplacementChange>,
    pub error: Option<String>,
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
        replacements.sort_by_key(|replacement| std::cmp::Reverse(replacement.from.chars().count()));

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
    let settings = dictionary_command_settings()?;
    let path = PathBuf::from(&settings.dictionary);
    match command {
        DictionaryCommand::Status => {
            let preview = if path.exists() {
                preview_dictionary(
                    &path,
                    Some(&settings.initial_prompt),
                    settings.dictionary_max_terms.parse().unwrap_or(80),
                    settings.dictionary_prompt_chars.parse().unwrap_or(1200),
                )?
            } else {
                let dictionary = Dictionary::default();
                DictionaryPreview {
                    path: path.clone(),
                    term_count: 0,
                    replacement_count: 0,
                    prompt: dictionary.build_prompt(
                        Some(&settings.initial_prompt),
                        settings.dictionary_max_terms.parse().unwrap_or(80),
                        settings.dictionary_prompt_chars.parse().unwrap_or(1200),
                    ),
                }
            };
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

fn dictionary_command_settings() -> Result<config::AppSettings> {
    let mut settings = config::load_settings()?;
    if let Some(paths) = env_paths("VOICEPI_DICTIONARY") {
        if let Some(path) = paths.first() {
            settings.dictionary = path.display().to_string();
        }
    }
    if let Some(enabled) = env_bool("VOICEPI_DICTIONARY_ENABLED") {
        settings.dictionary_enabled = enabled;
    }
    if let Some(value) = env_usize("VOICEPI_DICTIONARY_MAX_TERMS") {
        settings.dictionary_max_terms = value.to_string();
    }
    if let Some(value) = env_usize("VOICEPI_DICTIONARY_PROMPT_CHARS") {
        settings.dictionary_prompt_chars = value.to_string();
    }
    Ok(settings)
}

pub fn handle_runtime() -> Result<()> {
    let request = read_runtime_request()?;
    let settings = RuntimeDictionarySettings::from_env_and_config();
    let result =
        runtime_dictionary_result(&settings, request.base_prompt.as_deref(), &request.text);
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

pub fn runtime_dictionary_result(
    settings: &RuntimeDictionarySettings,
    base_prompt: Option<&str>,
    text: &str,
) -> RuntimeDictionaryResult {
    let path = settings
        .paths
        .first()
        .map(|path| path.display().to_string());
    if !settings.enabled {
        let dictionary = Dictionary::default();
        return RuntimeDictionaryResult {
            enabled: false,
            path,
            loaded_paths: Vec::new(),
            term_count: 0,
            replacement_count: 0,
            terms: Vec::new(),
            all_terms: Vec::new(),
            replacements: Vec::new(),
            prompt: dictionary.build_prompt(base_prompt, settings.max_terms, settings.max_chars),
            text: text.to_owned(),
            changes: Vec::new(),
            error: None,
        };
    }

    let (dictionary, loaded_paths, mut error) = load_runtime_dictionary(&settings.paths);
    let terms = dictionary.prompt_terms(settings.max_terms, settings.max_chars);
    let prompt = dictionary.build_prompt(base_prompt, settings.max_terms, settings.max_chars);
    let all_terms = dictionary.terms.clone();
    let replacements = dictionary.replacements.clone();
    let (text, changes) = match dictionary.apply_replacements(text) {
        Ok(result) => result,
        Err(err) => {
            append_error(&mut error, err.to_string());
            (text.to_owned(), Vec::new())
        }
    };

    RuntimeDictionaryResult {
        enabled: true,
        path,
        loaded_paths: loaded_paths
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
        term_count: dictionary.terms.len(),
        replacement_count: dictionary.replacements.len(),
        terms,
        all_terms,
        replacements,
        prompt,
        text,
        changes,
        error,
    }
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

fn load_runtime_dictionary(paths: &[PathBuf]) -> (Dictionary, Vec<PathBuf>, Option<String>) {
    let mut dictionary = Dictionary::default();
    let mut loaded_paths = Vec::new();
    let mut error = None;

    for path in paths {
        if !path.exists() {
            continue;
        }
        match load_dictionary(path) {
            Ok(next) => {
                merge_dictionary(&mut dictionary, next);
                loaded_paths.push(path.clone());
            }
            Err(err) => append_error(&mut error, format!("{}: {err}", path.display())),
        }
    }

    dictionary.terms = dedupe_terms(dictionary.terms);
    (dictionary, loaded_paths, error)
}

fn merge_dictionary(into: &mut Dictionary, next: Dictionary) {
    into.terms.extend(next.terms);
    for replacement in next.replacements {
        if let Some(existing) = into
            .replacements
            .iter_mut()
            .find(|existing| existing.from == replacement.from)
        {
            existing.to = replacement.to;
        } else {
            into.replacements.push(replacement);
        }
    }
}

fn append_error(target: &mut Option<String>, message: String) {
    if message.trim().is_empty() {
        return;
    }
    match target {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&message);
        }
        None => *target = Some(message),
    }
}

fn env_bool(name: &str) -> Option<bool> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .map(|value| !matches!(value.as_str(), "0" | "false" | "no" | "off"))
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
}

fn env_paths(name: &str) -> Option<Vec<PathBuf>> {
    let value = env::var_os(name)?;
    let paths = env::split_paths(&value)
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    (!paths.is_empty()).then_some(paths)
}

fn read_runtime_request() -> Result<RuntimeRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
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
    fn runtime_dictionary_applies_prompt_terms_and_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        std::fs::write(
            &path,
            r#"{"terms":["Slack","Claude Code","Codex"],"replacements":{"Cloud Code":"Claude Code","code X":"Codex"}}"#,
        )
        .unwrap();
        let settings = RuntimeDictionarySettings::new(true, vec![path.clone()], 10, 1200);

        let result = runtime_dictionary_result(
            &settings,
            Some("Base prompt"),
            "Open Cloud Code and code X.",
        );

        assert!(result.enabled);
        let expected_path = path.display().to_string();
        assert_eq!(result.path.as_deref(), Some(expected_path.as_str()));
        assert_eq!(result.loaded_paths, vec![path.display().to_string()]);
        assert_eq!(result.term_count, 3);
        assert_eq!(result.replacement_count, 2);
        assert_eq!(result.terms, vec!["Slack", "Claude Code", "Codex"]);
        assert_eq!(result.all_terms, vec!["Slack", "Claude Code", "Codex"]);
        assert_eq!(
            result.prompt.as_deref(),
            Some("Base prompt\nVocabulary: Slack, Claude Code, Codex")
        );
        assert_eq!(result.text, "Open Claude Code and Codex.");
        assert_eq!(
            result.changes,
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
            ]
        );
        assert_eq!(result.error, None);
    }

    #[test]
    fn runtime_dictionary_disabled_preserves_base_prompt_and_text() {
        let settings =
            RuntimeDictionarySettings::new(false, vec![PathBuf::from("dictionary.json")], 10, 1200);

        let result = runtime_dictionary_result(&settings, Some("Base prompt"), "Cloud Code");

        assert!(!result.enabled);
        assert_eq!(result.prompt.as_deref(), Some("Base prompt"));
        assert_eq!(result.text, "Cloud Code");
        assert!(result.terms.is_empty());
        assert!(result.all_terms.is_empty());
        assert!(result.changes.is_empty());
    }

    #[test]
    fn runtime_dictionary_missing_file_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let settings = RuntimeDictionarySettings::new(true, vec![missing.clone()], 10, 1200);

        let result = runtime_dictionary_result(&settings, Some("Base prompt"), "Cloud Code");

        let expected_path = missing.display().to_string();
        assert_eq!(result.path.as_deref(), Some(expected_path.as_str()));
        assert!(result.loaded_paths.is_empty());
        assert_eq!(result.prompt.as_deref(), Some("Base prompt"));
        assert_eq!(result.text, "Cloud Code");
        assert_eq!(result.error, None);
    }

    #[test]
    fn runtime_dictionary_reports_parse_errors_without_rewriting_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        std::fs::write(&path, "{not json").unwrap();
        let settings = RuntimeDictionarySettings::new(true, vec![path], 10, 1200);

        let result = runtime_dictionary_result(&settings, Some("Base prompt"), "Cloud Code");

        assert_eq!(result.prompt.as_deref(), Some("Base prompt"));
        assert_eq!(result.text, "Cloud Code");
        assert!(result.error.unwrap().contains("dictionary.json"));
    }

    #[test]
    fn runtime_dictionary_merges_paths_and_later_replacements_win() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.txt");
        std::fs::write(
            &first,
            r#"{"terms":["Codex"],"replacements":{"code X":"wrong"}}"#,
        )
        .unwrap();
        std::fs::write(
            &second,
            "terms:\n- Claude Code\nreplacements:\ncode X => Codex\n",
        )
        .unwrap();
        let settings = RuntimeDictionarySettings::new(true, vec![first, second], 10, 1200);

        let result = runtime_dictionary_result(&settings, None, "try code X");

        assert_eq!(result.terms, vec!["Codex", "Claude Code"]);
        assert_eq!(result.text, "try Codex");
        assert_eq!(
            result.replacements,
            vec![Replacement {
                from: "code X".to_owned(),
                to: "Codex".to_owned(),
            }]
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
