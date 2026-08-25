//! `dictionary` CLI command handlers.
//!
//! The hidden `dictionary-runtime` JSON-on-stdin RPC lives in
//! [`super::runtime_request`]. The user-facing commands here (`status`,
//! `open`, `add`, and `replace`) share the same effective dictionary path and
//! environment/config precedence as that RPC.

use std::path::PathBuf;

use crate::cli::DictionaryCommand;
use crate::config;
use anyhow::Result;

pub(crate) use super::runtime_loader::load_session_dictionary_with;
pub use super::runtime_loader::{
    load_session_dictionary, DictionaryProvider, ReloadPrecedence, ReloadingDictionary,
    SessionDictionary, StaticDictionary,
};
use super::store::load_dictionary;
use super::{env_bool, env_usize, Dictionary};

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
                dictionary_path: resolved_dictionary_argument(dictionary, &settings.dictionary),
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
            super::prompt::handle_prompt_with_default(
                dictionary,
                &settings.dictionary,
                json,
                max_length,
            )?;
        }
        DictionaryCommand::List { dictionary, json } => {
            super::prompt::handle_list_with_default(dictionary, &settings.dictionary, json)?;
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
                dictionary_path: resolved_dictionary_argument(dictionary, &settings.dictionary),
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
                dictionary_path: resolved_dictionary_argument(dictionary, &settings.dictionary),
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

pub(super) fn resolved_dictionary_argument(
    argument: Option<String>,
    fallback: &str,
) -> Option<String> {
    argument.or_else(|| Some(fallback.to_owned()))
}

/// Public re-export of the private `dictionary_command_settings` helper so
/// the sibling `prompt` module can reuse the exact env / config precedence
/// used by `dictionary status`. Kept as a distinct name to make the
/// coupling obvious from `prompt.rs`.
pub(super) fn dictionary_command_settings_for_prompt() -> Result<config::AppSettings> {
    dictionary_command_settings()
}

pub(super) fn dictionary_command_settings() -> Result<config::AppSettings> {
    let mut settings = config::load_settings()?;
    let resolved = config::effective_runtime_config();
    settings.dictionary = match resolved.get("dictionary") {
        Some(paths) => std::env::split_paths(paths)
            .find(|path| !path.as_os_str().is_empty())
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        None => super::store::default_dictionary_path()
            .display()
            .to_string(),
    };
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
