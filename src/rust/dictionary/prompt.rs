//! `dictionary prompt` and `dictionary list` CLI adapters.
//!
//! Audit item 2 chunk C — thin wrappers around the existing dictionary +
//! prompt-building library that print, on stdout, the Whisper
//! `initial_prompt` string (and, for `list`, the raw term / replacement
//! tables) that the runtime would use for a given dictionary + settings.
//!
//! Useful for verifying that a term / replacement list produces a sane
//! prompt without spinning up the Python worker, and for the Wayland smoke
//! script to prove the dictionary → prompt pipeline is wired up in the
//! shipped binary.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::Serialize;

use super::store::{load_dictionary, resolve_dictionary_path};
use super::{Dictionary, Replacement};

/// JSON payload emitted by `dictionary prompt --json`. Kept small and
/// stable — external callers may grep against these keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptJson {
    /// The full Whisper `initial_prompt` string (empty when both the
    /// base prompt and the vocabulary are empty).
    pub prompt: String,
    /// Character count of `prompt`.
    pub length_chars: usize,
    /// Number of vocabulary terms that made it into `prompt`.
    pub term_count: usize,
    /// True when the term-count / character cap dropped one or more
    /// terms from the on-disk dictionary.
    pub truncated: bool,
    /// The absolute path the dictionary was read from (or would have
    /// been read from if it did not exist).
    pub source: String,
}

/// JSON payload emitted by `dictionary list --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListJson {
    pub source: String,
    pub term_count: usize,
    pub replacement_count: usize,
    pub terms: Vec<String>,
    pub replacements: Vec<Replacement>,
}

/// Effective settings feeding the prompt build. Broken out so the pure
/// build step is unit-testable without going through env / config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSettings {
    pub base_prompt: String,
    pub max_terms: usize,
    pub max_chars: usize,
}

/// The pure "dictionary + settings → prompt" result. Callers turn this
/// into either plain-text or JSON output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltPrompt {
    pub prompt: String,
    pub length_chars: usize,
    pub term_count: usize,
    pub truncated: bool,
}

/// Build the Whisper `initial_prompt` for `dictionary` under `settings`.
/// Pure function — no IO, no env reads — so the invariants (empty ⇒
/// empty, cap ⇒ truncated) are unit-testable in isolation.
pub fn build_prompt(dictionary: &Dictionary, settings: &PromptSettings) -> BuiltPrompt {
    let base = settings.base_prompt.trim();
    let base_opt = (!base.is_empty()).then_some(base);
    let picked = dictionary.prompt_terms(settings.max_terms, settings.max_chars);
    let term_count = picked.len();
    let truncated = term_count < dictionary.terms.len();
    let prompt = dictionary
        .build_prompt(base_opt, settings.max_terms, settings.max_chars)
        .unwrap_or_default();
    let length_chars = prompt.chars().count();
    BuiltPrompt {
        prompt,
        length_chars,
        term_count,
        truncated,
    }
}

/// Resolve the on-disk dictionary path a `prompt` / `list` call should
/// read from: explicit `--dictionary` flag wins, then `VOICEPI_DICTIONARY`,
/// then the per-user default.
pub fn resolve_source(dictionary_arg: Option<&str>) -> Result<PathBuf> {
    resolve_dictionary_path(dictionary_arg)
}

/// Load a dictionary file for `prompt` / `list`.
///
/// * When `explicit` is `true` (user passed `--dictionary`), a missing
///   file is a hard error — the user asked for this specific file, so
///   silently swapping in an empty dictionary would hide typos.
/// * When `explicit` is `false` (falling back to the per-user default),
///   a missing file returns an empty dictionary. This mirrors the
///   runtime — a fresh install has no dictionary yet, and the Wayland
///   smoke script relies on that returning valid empty JSON rather
///   than failing.
///
/// Any other IO / parse failure is propagated.
pub fn load_or_empty(path: &Path, explicit: bool) -> Result<Dictionary> {
    if !path.exists() {
        if explicit {
            return Err(anyhow!("dictionary file not found: {}", path.display()));
        }
        return Ok(Dictionary::default());
    }
    load_dictionary(path)
}

/// Compose the effective [`PromptSettings`] for a `dictionary prompt` call:
/// starts from `config.json` + env-var overrides (same precedence the
/// runtime uses), then applies the `--max-length` flag when supplied.
pub fn effective_settings(max_length_arg: Option<usize>) -> Result<PromptSettings> {
    let settings = super::runtime::dictionary_command_settings_for_prompt()?;
    let max_terms = settings.dictionary_max_terms.parse::<usize>().unwrap_or(80);
    let max_chars = max_length_arg.unwrap_or_else(|| {
        settings
            .dictionary_prompt_chars
            .parse::<usize>()
            .unwrap_or(1200)
    });
    Ok(PromptSettings {
        base_prompt: settings.initial_prompt,
        max_terms,
        max_chars,
    })
}

/// Handle the user-facing `dictionary prompt` subcommand.
pub fn handle_prompt(
    dictionary_arg: Option<String>,
    json: bool,
    max_length: Option<usize>,
) -> Result<()> {
    let explicit = dictionary_arg.is_some();
    let source = resolve_source(dictionary_arg.as_deref())?;
    let settings = effective_settings(max_length)?;
    let dictionary = load_or_empty(&source, explicit)?;
    let built = build_prompt(&dictionary, &settings);
    if json {
        let payload = PromptJson {
            prompt: built.prompt,
            length_chars: built.length_chars,
            term_count: built.term_count,
            truncated: built.truncated,
            source: source.display().to_string(),
        };
        println!("{}", serde_json::to_string(&payload)?);
    } else {
        // Plain-text mode: print the raw prompt (or a blank line when
        // both the base prompt and the vocabulary are empty). The caller
        // can pipe this into another tool without stripping headers.
        println!("{}", built.prompt);
    }
    Ok(())
}

/// Handle the user-facing `dictionary list` subcommand.
pub fn handle_list(dictionary_arg: Option<String>, json: bool) -> Result<()> {
    let explicit = dictionary_arg.is_some();
    let source = resolve_source(dictionary_arg.as_deref())?;
    let dictionary = load_or_empty(&source, explicit)?;
    if json {
        let payload = ListJson {
            source: source.display().to_string(),
            term_count: dictionary.terms.len(),
            replacement_count: dictionary.replacements.len(),
            terms: dictionary.terms.clone(),
            replacements: dictionary.replacements.clone(),
        };
        println!("{}", serde_json::to_string(&payload)?);
    } else {
        println!("source: {}", source.display());
        println!("terms ({}):", dictionary.terms.len());
        for term in &dictionary.terms {
            println!("  {term}");
        }
        println!("replacements ({}):", dictionary.replacements.len());
        for replacement in &dictionary.replacements {
            println!("  {} => {}", replacement.from, replacement.to);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(base: &str, max_terms: usize, max_chars: usize) -> PromptSettings {
        PromptSettings {
            base_prompt: base.to_owned(),
            max_terms,
            max_chars,
        }
    }

    #[test]
    fn build_prompt_empty_dictionary_and_no_base_returns_empty() {
        let dictionary = Dictionary::default();
        let built = build_prompt(&dictionary, &settings("", 80, 1200));
        assert_eq!(built.prompt, "");
        assert_eq!(built.length_chars, 0);
        assert_eq!(built.term_count, 0);
        assert!(!built.truncated);
    }

    #[test]
    fn build_prompt_returns_base_only_when_no_terms() {
        let dictionary = Dictionary::default();
        let built = build_prompt(&dictionary, &settings("Base prompt", 80, 1200));
        assert_eq!(built.prompt, "Base prompt");
        assert_eq!(built.length_chars, "Base prompt".chars().count());
        assert_eq!(built.term_count, 0);
        assert!(!built.truncated);
    }

    #[test]
    fn build_prompt_appends_vocabulary_and_counts_all_picked_terms() {
        let dictionary = Dictionary {
            terms: vec![
                "Slack".to_owned(),
                "Claude Code".to_owned(),
                "Codex".to_owned(),
            ],
            replacements: Vec::new(),
        };
        let built = build_prompt(&dictionary, &settings("Base prompt", 80, 1200));
        assert_eq!(
            built.prompt,
            "Base prompt\nVocabulary: Slack, Claude Code, Codex"
        );
        assert_eq!(built.term_count, 3);
        assert!(!built.truncated);
    }

    #[test]
    fn build_prompt_respects_char_cap_and_reports_truncated() {
        let dictionary = Dictionary {
            terms: vec![
                "Slack".to_owned(),
                "Claude Code".to_owned(),
                "Codex".to_owned(),
            ],
            replacements: Vec::new(),
        };
        // Char cap fits only "Slack, Claude Code" (18 chars in the
        // prompt_terms accounting — see Dictionary::prompt_terms).
        let built = build_prompt(&dictionary, &settings("", 80, 18));
        assert_eq!(built.prompt, "Vocabulary: Slack, Claude Code");
        assert_eq!(built.term_count, 2);
        assert!(built.truncated);
    }

    #[test]
    fn build_prompt_respects_term_cap_and_reports_truncated() {
        let dictionary = Dictionary {
            terms: vec![
                "Slack".to_owned(),
                "Claude Code".to_owned(),
                "Codex".to_owned(),
            ],
            replacements: Vec::new(),
        };
        let built = build_prompt(&dictionary, &settings("", 2, 1200));
        assert_eq!(built.prompt, "Vocabulary: Slack, Claude Code");
        assert_eq!(built.term_count, 2);
        assert!(built.truncated);
    }

    #[test]
    fn build_prompt_replacements_do_not_appear_in_prompt() {
        // Replacements are post-STT and MUST NOT leak into the Whisper
        // initial_prompt (that would confuse the language model without
        // adding vocabulary hints).
        let dictionary = Dictionary {
            terms: vec!["Codex".to_owned()],
            replacements: vec![Replacement {
                from: "code X".to_owned(),
                to: "Codex".to_owned(),
            }],
        };
        let built = build_prompt(&dictionary, &settings("", 80, 1200));
        assert_eq!(built.prompt, "Vocabulary: Codex");
        assert!(!built.prompt.contains("code X"));
        assert!(!built.prompt.contains("=>"));
    }

    #[test]
    fn load_or_empty_default_returns_empty_for_missing_path() {
        // Fresh install: no dictionary yet, default path fallback must
        // succeed with an empty dictionary so `dictionary prompt` on a
        // clean box does not fail the Wayland smoke.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.json");
        let dictionary = load_or_empty(&missing, false).unwrap();
        assert!(dictionary.terms.is_empty());
        assert!(dictionary.replacements.is_empty());
    }

    #[test]
    fn load_or_empty_explicit_errors_for_missing_path() {
        // `--dictionary /nope.json`: user asked for a specific file; a
        // silent empty-dictionary would hide typos and make the tool
        // misleading, so surface the missing-file case as a real error.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.json");
        let err = load_or_empty(&missing, true).unwrap_err();
        let message = format!("{err}");
        assert!(
            message.contains("not found"),
            "error should mention 'not found': {message}"
        );
        assert!(
            message.contains("does-not-exist.json"),
            "error should include the path: {message}"
        );
    }

    #[test]
    fn load_or_empty_reads_terms_and_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        std::fs::write(
            &path,
            r#"{"terms":["Codex","Claude Code"],"replacements":{"code X":"Codex"}}"#,
        )
        .unwrap();
        let dictionary = load_or_empty(&path, true).unwrap();
        assert_eq!(dictionary.terms, vec!["Codex", "Claude Code"]);
        assert_eq!(dictionary.replacements.len(), 1);
    }

    #[test]
    fn resolve_source_explicit_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        let resolved = resolve_source(Some(path.to_string_lossy().as_ref())).unwrap();
        assert_eq!(resolved, path);
    }

    #[test]
    fn prompt_json_serializes_stable_key_shape() {
        // Guard: external callers grep against these key names, so a
        // rename would be a silent breaking change.
        let payload = PromptJson {
            prompt: "Vocabulary: Slack".to_owned(),
            length_chars: 17,
            term_count: 1,
            truncated: false,
            source: "/tmp/d.json".to_owned(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"prompt\":"));
        assert!(json.contains("\"length_chars\":17"));
        assert!(json.contains("\"term_count\":1"));
        assert!(json.contains("\"truncated\":false"));
        assert!(json.contains("\"source\":"));
    }
}
