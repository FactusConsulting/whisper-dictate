//! Custom dictionary support: parse + apply the user's prompt vocabulary and
//! deterministic replacement table. Split into sub-modules to keep each file
//! ≤500 LOC (see the file-by-file responsibility map in issue #348):
//!
//! * `parse` – JSON / plain-text parsers for the dictionary on-disk shape
//! * `store` – disk IO: path resolution, sanitisation, ensure / add / write
//! * `runtime` – `dictionary` + `dictionary-runtime` CLI command handlers and
//!   the `RuntimeDictionarySettings` env/config plumbing
//! * `training` – pure corpus-mining helpers plus the `build-from-corpus` /
//!   `suggest-terms` CLI adapters. This module is the shipping implementation
//!   for the dictionary training features — the Python parity code was
//!   retired in audit item 4 (see `docs/architecture-audit-2026-07-16.md`).
//! * `suggest` – fuzzy replacement suggestions (Ratcliff–Obershelp ratio)
//!   plus the `suggest-replacements` CLI adapter — likewise the sole
//!   implementation post audit item 4.

use std::collections::HashSet;
use std::env;
use std::path::PathBuf;

use anyhow::Result;
use regex::RegexBuilder;
use serde::Serialize;

mod parse;
mod prompt;
mod runtime;
mod store;
mod suggest;
mod training;

pub use parse::{parse_dictionary, parse_json_dictionary, parse_text_dictionary};
pub use prompt::{
    build_prompt, effective_settings, handle_list, handle_prompt, load_or_empty, resolve_source,
    BuiltPrompt, ListJson, PromptJson, PromptSettings,
};
pub use runtime::{
    handle_command, handle_runtime, preview_dictionary, runtime_dictionary_result,
    DictionaryPreview, RuntimeDictionaryResult, RuntimeDictionarySettings,
};
pub use store::{
    add_replacement, add_term, default_dictionary_path, ensure_json_dictionary, load_dictionary,
    load_dictionary_document, resolve_dictionary_path, sanitize_dictionary_path,
    terms_from_document, write_terms,
};
pub use suggest::{
    run_suggest_replacements, suggest_replacements_from_rows, ReplacementSuggestion,
};
pub use training::{
    extract_candidate_terms, merge_terms, run_build_from_corpus, run_suggest_from_misses,
    suggest_terms_from_misses, BuildFromCorpusOptions, MergePreview, SuggestFromMissesOptions,
    TermCandidate, TermSuggestion,
};

/// The parsed dictionary: prompt vocabulary `terms` and deterministic
/// `replacements` applied to the transcript.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dictionary {
    pub terms: Vec<String>,
    pub replacements: Vec<Replacement>,
}

/// A single deterministic replacement (case-insensitive, whole-word, applied
/// longest-first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Replacement {
    pub from: String,
    pub to: String,
}

/// A successful replacement and how many times it fired in one transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplacementChange {
    pub from: String,
    pub to: String,
    pub count: usize,
}

impl Dictionary {
    /// Pick the prefix of `terms` that fits inside the term-count + character
    /// budget. Used to build the Whisper `initial_prompt`. Terms past either
    /// cap are dropped (the prompt is short by design — extra terms past the
    /// cap stop helping and start eating into the audio context window).
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

    /// Build the Whisper `initial_prompt` from the user's base prompt and the
    /// budget-fitted vocabulary terms. Returns `None` when both are empty so
    /// the caller can pass through the empty string instead of " ".
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

    /// Apply every replacement to `text` case-insensitively, whole-word, in
    /// **longest-from-first** order so "Cloud Code" rewrites before "Code".
    /// Returns the rewritten text plus the per-replacement hit counts (for
    /// telemetry / the dictation overlay).
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

/// Deduplicate term strings case-insensitively while preserving insertion
/// order. The first surface form for a given casefolded key wins so user
/// capitalisation is preserved.
pub(crate) fn dedupe_terms(terms: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for term in terms {
        let term = term.trim();
        let key = term.to_lowercase();
        if !term.is_empty() && seen.insert(key) {
            out.push(term.to_owned());
        }
    }
    out
}

pub(crate) fn env_bool(name: &str) -> Option<bool> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .map(|value| !matches!(value.as_str(), "0" | "false" | "no" | "off"))
}

pub(crate) fn env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
}

pub(crate) fn env_paths(name: &str) -> Option<Vec<PathBuf>> {
    let value = env::var_os(name)?;
    let paths = env::split_paths(&value)
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    (!paths.is_empty()).then_some(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

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
