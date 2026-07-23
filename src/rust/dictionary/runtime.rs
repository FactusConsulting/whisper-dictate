//! `dictionary` and `dictionary-runtime` CLI command handlers.
//!
//! `dictionary` exposes the user-facing read/add operations
//! (`status`, `open`, `add`, `replace`). `dictionary-runtime` is the hidden
//! JSON-on-stdin RPC the Python worker calls to build the Whisper
//! `initial_prompt` and apply post-STT replacements without going through the
//! Python parser. Both go through [`RuntimeDictionarySettings`] which reads
//! env vars first (`VOICEPI_DICTIONARY*`) then `config.json` so the user can
//! override anything from the shell.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cli::DictionaryCommand;
use crate::config;

use super::store::load_dictionary;
use super::{env_bool, env_paths, env_usize, Dictionary, Replacement, ReplacementChange};

#[derive(Debug, Deserialize)]
struct RuntimeRequest {
    #[serde(default)]
    base_prompt: Option<String>,
    #[serde(default)]
    text: String,
}

/// Effective settings used by the `dictionary-runtime` handler. Env vars win
/// over `config.json`; missing values fall back to the defaults baked into the
/// Python side so the Python and Rust runtimes stay byte-identical.
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

/// The wire-format response from `dictionary-runtime` (and the in-process
/// equivalent [`runtime_dictionary_result`]).
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

/// Status preview emitted by the `dictionary status` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryPreview {
    pub path: PathBuf,
    pub term_count: usize,
    pub replacement_count: usize,
    pub prompt: Option<String>,
}

/// Build a [`DictionaryPreview`] for `path` against the given prompt + budgets.
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

/// Dispatch table for the user-facing `dictionary` subcommands.
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
            let added = super::store::add_term(&path, &term)?;
            println!(
                "{}: {}",
                if added { "added" } else { "already present" },
                path.display()
            );
        }
        DictionaryCommand::Replace { mapping } => {
            let (from, to, changed) = super::store::add_replacement(&path, &mapping)?;
            println!(
                "{}: {from} => {to} ({})",
                if changed { "saved" } else { "unchanged" },
                path.display()
            );
        }
        DictionaryCommand::BuildFromCorpus {
            benchmark_corpus,
            app_root,
            dictionary,
            language,
            category,
            min_count,
            apply,
            json,
        } => {
            let opts = super::training::BuildFromCorpusOptions {
                corpus_manifest: benchmark_corpus,
                app_root: app_root.map(PathBuf::from),
                appdata: Some(config::platform_config_dir()),
                dictionary_path: dictionary,
                language,
                category,
                min_count,
                apply,
                as_json: json,
            };
            let rc = super::training::run_build_from_corpus(opts);
            if rc != 0 {
                std::process::exit(rc);
            }
        }
        DictionaryCommand::Prompt {
            dictionary,
            json,
            max_length,
        } => {
            super::prompt::handle_prompt(dictionary, json, max_length)?;
        }
        DictionaryCommand::List { dictionary, json } => {
            super::prompt::handle_list(dictionary, json)?;
        }
        DictionaryCommand::SuggestTerms {
            jsonl,
            dictionary,
            min_count,
            apply,
            json,
        } => {
            let opts = super::training::SuggestFromMissesOptions {
                jsonl_path: PathBuf::from(jsonl),
                dictionary_path: dictionary,
                min_count,
                apply,
                as_json: json,
            };
            let rc = super::training::run_suggest_from_misses(opts);
            if rc != 0 {
                std::process::exit(rc);
            }
        }
        DictionaryCommand::SuggestReplacements {
            jsonl,
            dictionary,
            min_confidence,
            json,
        } => {
            let opts = super::suggest::SuggestReplacementsOptions {
                jsonl_path: jsonl,
                dictionary_path: dictionary,
                min_confidence,
                as_json: json,
            };
            let rc = super::suggest::run_suggest_replacements(opts);
            if rc != 0 {
                std::process::exit(rc);
            }
        }
    }
    Ok(())
}

/// Public re-export of the private `dictionary_command_settings` helper so
/// the sibling `prompt` module can reuse the exact env / config precedence
/// used by `dictionary status`. Kept as a distinct name to make the
/// coupling obvious from `prompt.rs`.
pub(super) fn dictionary_command_settings_for_prompt() -> Result<config::AppSettings> {
    dictionary_command_settings()
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

/// Read a JSON request from stdin, build the prompt + apply replacements, then
/// print the JSON response on stdout. Used by the Python worker to skip its
/// own dictionary loader when the Rust binary is available.
pub fn handle_runtime() -> Result<()> {
    let request = read_runtime_request()?;
    let settings = RuntimeDictionarySettings::from_env_and_config();
    let result =
        runtime_dictionary_result(&settings, request.base_prompt.as_deref(), &request.text);
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

/// In-process equivalent of [`handle_runtime`] — same shape, but the caller
/// supplies the settings + request directly (used by unit tests).
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

/// The dictionary state a live in-process session needs: the loaded
/// [`Dictionary`] (for the replacement table) plus the resolved prompt-budget
/// knobs (for the Whisper `initial_prompt`). Built from the same
/// `VOICEPI_DICTIONARY*` env + `config.json` the `dictionary-runtime` RPC and
/// the Python worker read, so the in-process Rust engine biases + rewrites
/// identically.
#[derive(Debug, Clone)]
pub struct SessionDictionary {
    /// The merged dictionary (empty when disabled or nothing loaded).
    pub dictionary: Dictionary,
    /// Prompt term-count budget (`VOICEPI_DICTIONARY_MAX_TERMS`).
    pub max_terms: usize,
    /// Prompt character budget (`VOICEPI_DICTIONARY_PROMPT_CHARS`).
    pub max_chars: usize,
    /// Whether the dictionary is enabled (`VOICEPI_DICTIONARY_ENABLED`).
    pub enabled: bool,
}

impl SessionDictionary {
    /// Build the Whisper `initial_prompt` from `base_prompt` + the
    /// budget-fitted vocabulary terms, or `None` when both are empty (the
    /// caller then passes the empty string through). Mirrors Python's
    /// `_dictionary_prompt_runtime`.
    pub fn initial_prompt(&self, base_prompt: Option<&str>) -> Option<String> {
        self.dictionary
            .build_prompt(base_prompt, self.max_terms, self.max_chars)
    }

    /// `true` when the loaded dictionary carries any replacements, so the
    /// session wiring can skip attaching the replacement seam otherwise.
    pub fn has_replacements(&self) -> bool {
        !self.dictionary.replacements.is_empty()
    }

    /// Fold the dictionary terms into an existing prompt `slot` in place: take
    /// the current base prompt out, rebuild it through [`Self::initial_prompt`],
    /// and write the (possibly `None`) result back. Collapses the identical
    /// "take → initial_prompt → store" dance each backend-config call site
    /// (cloud `prompt`, local `initial_prompt`) would otherwise repeat, so the
    /// prompt-biasing wiring lives in exactly one place.
    pub fn fold_into_prompt(&self, slot: &mut Option<String>) {
        let base = slot.take();
        *slot = self.initial_prompt(base.as_deref());
    }
}

/// Load the [`SessionDictionary`] from the process env + `config.json`, the
/// single entry the in-process session uses for BOTH halves of dictionary
/// support: term-based prompt biasing ([`SessionDictionary::initial_prompt`])
/// and the replacement table ([`SessionDictionary::dictionary`]). When
/// disabled, returns an empty dictionary so both halves are no-ops.
pub fn load_session_dictionary() -> SessionDictionary {
    let settings = RuntimeDictionarySettings::from_env_and_config();
    let dictionary = if settings.enabled {
        load_runtime_dictionary(&settings.paths).0
    } else {
        Dictionary::default()
    };
    SessionDictionary {
        dictionary,
        max_terms: settings.max_terms,
        max_chars: settings.max_chars,
        enabled: settings.enabled,
    }
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

    dictionary.terms = super::dedupe_terms(dictionary.terms);
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

fn read_runtime_request() -> Result<RuntimeRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

// Keep the legacy `Path` import referenced for future-proofing callers that
// reach into the module's internals via `dictionary::runtime`.
#[allow(dead_code)]
fn _path_marker(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_dictionary_builds_prompt_and_reports_replacements() {
        // Pure: `initial_prompt` fits the base prompt + budget-limited terms,
        // and `has_replacements` reflects the table -- no env, no I/O.
        let sd = SessionDictionary {
            dictionary: Dictionary {
                terms: vec!["Codex".to_owned(), "Claude Code".to_owned()],
                replacements: vec![Replacement {
                    from: "code x".to_owned(),
                    to: "Codex".to_owned(),
                }],
            },
            max_terms: 80,
            max_chars: 1200,
            enabled: true,
        };
        assert!(sd.has_replacements());
        let prompt = sd
            .initial_prompt(Some("base hint"))
            .expect("prompt present");
        assert!(prompt.contains("base hint"), "{prompt}");
        assert!(
            prompt.contains("Vocabulary: Codex, Claude Code"),
            "{prompt}"
        );

        let empty = SessionDictionary {
            dictionary: Dictionary::default(),
            max_terms: 80,
            max_chars: 1200,
            enabled: false,
        };
        assert!(!empty.has_replacements());
        assert_eq!(empty.initial_prompt(None), None);
    }

    #[test]
    fn fold_into_prompt_folds_terms_and_clears_when_empty() {
        // With terms: the slot's base prompt is rebuilt to base + vocabulary.
        let sd = SessionDictionary {
            dictionary: Dictionary {
                terms: vec!["Codex".to_owned()],
                replacements: Vec::new(),
            },
            max_terms: 80,
            max_chars: 1200,
            enabled: true,
        };
        let mut slot = Some("base hint".to_owned());
        sd.fold_into_prompt(&mut slot);
        let folded = slot.expect("prompt present");
        assert!(folded.contains("base hint"), "{folded}");
        assert!(folded.contains("Vocabulary: Codex"), "{folded}");

        // A term-less base still folds through initial_prompt: the base
        // prompt survives on its own (no vocabulary line to append).
        let bare = SessionDictionary {
            dictionary: Dictionary::default(),
            max_terms: 80,
            max_chars: 1200,
            enabled: true,
        };
        let mut only_base = Some("keep me".to_owned());
        bare.fold_into_prompt(&mut only_base);
        assert_eq!(only_base.as_deref(), Some("keep me"));

        // Empty base + no terms collapses the slot to None (the caller then
        // passes the empty string through to the endpoint).
        let mut empty = None;
        bare.fold_into_prompt(&mut empty);
        assert_eq!(empty, None);
    }

    #[test]
    fn load_session_dictionary_reads_env_dictionary() {
        // Env-driven load: `VOICEPI_DICTIONARY` + `VOICEPI_DICTIONARY_ENABLED`
        // point at a temp file; the loaded terms + replacements come back on
        // the SessionDictionary. Serialised via the crate-wide ENV_LOCK.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.json");
        std::fs::write(
            &path,
            r#"{"terms":["Codex"],"replacements":{"code x":"Codex"}}"#,
        )
        .unwrap();
        std::env::set_var("VOICEPI_DICTIONARY", &path);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");
        let sd = load_session_dictionary();
        std::env::remove_var("VOICEPI_DICTIONARY");
        std::env::remove_var("VOICEPI_DICTIONARY_ENABLED");

        assert!(sd.enabled);
        assert!(sd.has_replacements());
        assert_eq!(sd.dictionary.terms, vec!["Codex".to_owned()]);
        let prompt = sd.initial_prompt(None).expect("prompt from terms");
        assert!(prompt.contains("Vocabulary: Codex"), "{prompt}");
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
}
