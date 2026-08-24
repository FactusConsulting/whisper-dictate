use super::*;
use crate::config;
use crate::dictionary::{Dictionary, Replacement};
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
