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
use std::path::PathBuf;

use crate::cli::DictionaryCommand;
use crate::config;
use anyhow::Result;
use serde::{Deserialize, Serialize};

pub(crate) use super::runtime_loader::load_session_dictionary_with;
use super::runtime_loader::{append_error, load_runtime_dictionary};
pub use super::runtime_loader::{
    load_session_dictionary, DictionaryProvider, ReloadPrecedence, ReloadingDictionary,
    SessionDictionary, StaticDictionary,
};
use super::runtime_settings::RuntimeDictionarySettings;
use super::store::load_dictionary;
use super::{env_bool, env_usize, Dictionary, Replacement, ReplacementChange};

#[derive(Debug, Deserialize)]
struct RuntimeRequest {
    #[serde(default)]
    base_prompt: Option<String>,
    #[serde(default)]
    text: String,
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

fn read_runtime_request() -> Result<RuntimeRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Snapshot every dictionary env var on construction and restore each to
    /// its prior value on drop -- Rust tests share one process env and run in
    /// arbitrary order, so a test that sets `VOICEPI_DICTIONARY*` must leave the
    /// environment exactly as it found it (restore-on-drop also fires during a
    /// panic, so a failed assertion can't leak). Hold alongside `ENV_LOCK`.
    struct DictEnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl DictEnvGuard {
        fn new() -> Self {
            // `VOICEPI_CONFIG` is included so config-first tests can point
            // `config::load_settings` at a temp config.json and have it restored
            // like the rest.
            let keys = [
                "VOICEPI_DICTIONARY",
                "VOICEPI_DICTIONARY_ENABLED",
                "VOICEPI_DICTIONARY_MAX_TERMS",
                "VOICEPI_DICTIONARY_PROMPT_CHARS",
                "VOICEPI_CONFIG",
            ];
            let saved = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            Self { saved }
        }
    }

    impl Drop for DictEnvGuard {
        fn drop(&mut self) {
            for (key, prior) in &self.saved {
                match prior {
                    Some(val) => std::env::set_var(key, val),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    /// Write a config.json at `config_path` whose dictionary points at
    /// `dict_path` with the given `enabled` flag (budgets left at their
    /// defaults), and export `VOICEPI_CONFIG` so `config::load_settings` reads
    /// it. Lets the config-first tests drive `dictionary` / `dictionary_enabled`
    /// through config.json -- the source of truth the live-reload now honours --
    /// rather than through env.
    fn write_dictionary_config(config_path: &Path, dict_path: &Path, enabled: bool) {
        let settings = config::AppSettings {
            dictionary: dict_path.display().to_string(),
            dictionary_enabled: enabled,
            ..config::AppSettings::default()
        };
        config::save_settings_to_path(&settings, config_path).expect("write temp config.json");
        std::env::set_var("VOICEPI_CONFIG", config_path);
    }

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

        // Snapshot/restore every dictionary var (restore fires on drop), and
        // pin the budgets explicitly rather than inheriting them: an external
        // `VOICEPI_DICTIONARY_MAX_TERMS=0` (or a tiny `_PROMPT_CHARS`) would
        // otherwise drop the vocabulary line and break the prompt assertion.
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.json");
        std::fs::write(
            &path,
            r#"{"terms":["Codex"],"replacements":{"code x":"Codex"}}"#,
        )
        .unwrap();
        std::env::set_var("VOICEPI_DICTIONARY", &path);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");
        std::env::set_var("VOICEPI_DICTIONARY_MAX_TERMS", "80");
        std::env::set_var("VOICEPI_DICTIONARY_PROMPT_CHARS", "1200");

        let sd = load_session_dictionary();

        assert!(sd.enabled);
        assert!(sd.has_replacements());
        assert_eq!(sd.dictionary.terms, vec!["Codex".to_owned()]);
        let prompt = sd.initial_prompt(None).expect("prompt from terms");
        assert!(prompt.contains("Vocabulary: Codex"), "{prompt}");
    }

    #[test]
    fn reloading_dictionary_picks_up_file_edits() {
        // Live-reload: a ReloadingDictionary re-reads the file at each `current`
        // call and reloads on a freshness/settings miss, so an edit to the
        // dictionary between utterances takes effect -- Python's per-utterance
        // `_dictionary_runtime`. The path is config-driven (the reload resolves
        // config-first), so no env dictionary vars are needed.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        write_dictionary_config(&dir.path().join("config.json"), &dict, true);

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::ConfigFirst);
        let (before, _) = provider
            .current()
            .apply_replacements("hello world")
            .unwrap();
        assert_eq!(before, "hi world");

        // Edit the file to a DIFFERENT byte length so the size component of the
        // freshness stamp flips deterministically (a same-length edit would
        // still be caught by the nanosecond mtime, but size makes the test
        // robust regardless of filesystem mtime granularity).
        std::fs::write(&dict, r#"{"replacements":{"hello":"HELLO"}}"#).unwrap();
        let (after, _) = provider
            .current()
            .apply_replacements("hello world")
            .unwrap();
        assert_eq!(after, "HELLO world");
    }

    #[test]
    fn reloading_dictionary_reflects_enabled_toggle() {
        // Disabling the dictionary in config.json (a Settings save, no restart)
        // flips the cache key's `enabled` field, so the next `current` reloads
        // to an empty (passthrough) table -- the `dictionary_enabled` live
        // setting takes effect without an app restart, resolved config-first.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        let config = dir.path().join("config.json");
        write_dictionary_config(&config, &dict, true);

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::ConfigFirst);
        let (enabled, _) = provider.current().apply_replacements("hello").unwrap();
        assert_eq!(enabled, "hi");

        // Re-save config.json with the dictionary disabled.
        write_dictionary_config(&config, &dict, false);
        let (disabled, _) = provider.current().apply_replacements("hello").unwrap();
        assert_eq!(
            disabled, "hello",
            "disabling the dictionary in config must reach the reload"
        );
    }

    #[test]
    fn reloading_dictionary_env_first_honours_env_path() {
        // The EnvFirst provider (used by the env-driven `simulate-session` verb
        // + the groq-cli smoke) resolves the dictionary from the
        // `VOICEPI_DICTIONARY*` env the worker exports, so an env-set dictionary
        // applies its replacements regardless of config.json.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hey"}}"#).unwrap();
        std::env::set_var("VOICEPI_DICTIONARY", &dict);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");
        // A config pointing elsewhere must NOT win under EnvFirst.
        std::env::remove_var("VOICEPI_CONFIG");

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::EnvFirst);
        let (out, _) = provider
            .current()
            .apply_replacements("hello world")
            .unwrap();
        assert_eq!(out, "hey world");
    }

    #[test]
    fn reloading_dictionary_reloads_terms_not_just_replacements() {
        // Term coverage: the reloaded table carries the file's `terms` too, and
        // a file edit updates them (not only the replacement map).
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(
            &dict,
            r#"{"terms":["Codex"],"replacements":{"cloud code":"Claude Code"}}"#,
        )
        .unwrap();
        write_dictionary_config(&dir.path().join("config.json"), &dict, true);

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::ConfigFirst);
        assert_eq!(provider.current().terms, vec!["Codex".to_owned()]);

        // Edit the file (different byte length) -> the reloaded terms update.
        std::fs::write(
            &dict,
            r#"{"terms":["Codex","Slack"],"replacements":{"cloud code":"Claude Code"}}"#,
        )
        .unwrap();
        assert_eq!(
            provider.current().terms,
            vec!["Codex".to_owned(), "Slack".to_owned()],
            "a file edit must reload the term list, not only replacements"
        );
    }

    #[test]
    fn reloading_dictionary_uses_readable_subset_on_partial_failure() {
        // Multiple configured files with one broken: the reload must keep the
        // readable subset's replacements rather than discarding everything, and
        // leave the key unadvanced so the broken file is retried.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.json");
        std::fs::write(
            &good,
            r#"{"terms":["Alpha","Beta"],"replacements":{"hello":"hi"}}"#,
        )
        .unwrap();
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{ not json").unwrap();
        let joined = std::env::join_paths([&good, &bad]).unwrap();
        std::env::set_var("VOICEPI_DICTIONARY", &joined);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");
        std::env::set_var("VOICEPI_DICTIONARY_MAX_TERMS", "1");
        std::env::set_var("VOICEPI_DICTIONARY_PROMPT_CHARS", "100");
        std::env::remove_var("VOICEPI_CONFIG");

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::EnvFirst);
        let (out, _) = provider
            .current()
            .apply_replacements("hello world")
            .unwrap();
        assert_eq!(
            out, "hi world",
            "the readable file's replacements must apply despite a broken sibling"
        );
        assert_eq!(provider.current().terms, ["Alpha", "Beta"]);
        assert_eq!(
            provider.initial_prompt(None).as_deref(),
            Some("Vocabulary: Alpha")
        );
        std::env::set_var("VOICEPI_DICTIONARY_MAX_TERMS", "80");
        std::env::set_var("VOICEPI_DICTIONARY_PROMPT_CHARS", "5");
        assert_eq!(
            provider.initial_prompt(None).as_deref(),
            Some("Vocabulary: Alpha")
        );
        let error = provider
            .take_load_error()
            .expect("the broken file must be reported");
        assert!(error.contains("bad.json"));
    }

    #[test]
    fn reloading_dictionary_clears_an_emptied_file_despite_broken_sibling() {
        // Regression for the content-based partial check: emptying the last
        // readable dictionary while a sibling is broken must clear its
        // replacements (a valid EMPTY subset is a successful load), not keep the
        // stale table because the merged result is now empty.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.json");
        std::fs::write(&a, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        let b = dir.path().join("b.json");
        std::fs::write(&b, r#"{"replacements":{}}"#).unwrap();
        let joined = std::env::join_paths([&a, &b]).unwrap();
        std::env::set_var("VOICEPI_DICTIONARY", &joined);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");
        std::env::remove_var("VOICEPI_CONFIG");

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::EnvFirst);
        let (before, _) = provider
            .current()
            .apply_replacements("hello world")
            .unwrap();
        assert_eq!(before, "hi world");

        // Empty the readable file AND break the sibling.
        std::fs::write(&a, r#"{"replacements":{}}"#).unwrap();
        std::fs::write(&b, "{ not json").unwrap();
        let (after, _) = provider
            .current()
            .apply_replacements("hello world")
            .unwrap();
        assert_eq!(
            after, "hello world",
            "emptying the readable file must clear replacements even with a broken sibling"
        );
    }

    #[test]
    fn reloading_dictionary_prompt_and_terms_reload_together() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"terms":["Codex"]}"#).unwrap();
        write_dictionary_config(&dir.path().join("config.json"), &dict, true);

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::ConfigFirst);
        let first = provider.initial_prompt_with_terms(Some("base"));
        assert_eq!(first.0.as_deref(), Some("base\nVocabulary: Codex"));
        assert_eq!(first.1, ["Codex"]);

        std::fs::write(&dict, r#"{"terms":["Codex","Slack"]}"#).unwrap();
        let second = provider.initial_prompt_with_terms(Some("base"));
        assert_eq!(second.0.as_deref(), Some("base\nVocabulary: Codex, Slack"),);
        assert_eq!(second.1, ["Codex", "Slack"]);
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
