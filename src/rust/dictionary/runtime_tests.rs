use super::{DictionaryProvider, ReloadPrecedence, ReloadingDictionary, RuntimeDictionarySettings};

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn dictionary(path: &std::path::Path) -> Self {
        let keys = [
            "VOICEPI_CONFIG",
            "VOICEPI_DICTIONARY",
            "VOICEPI_DICTIONARY_ENABLED",
            "VOICEPI_DICTIONARY_MAX_TERMS",
            "VOICEPI_DICTIONARY_PROMPT_CHARS",
        ];
        let saved = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        std::env::remove_var("VOICEPI_CONFIG");
        std::env::set_var("VOICEPI_DICTIONARY", path);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");
        std::env::set_var("VOICEPI_DICTIONARY_MAX_TERMS", "1");
        std::env::set_var("VOICEPI_DICTIONARY_PROMPT_CHARS", "1200");
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[test]
fn reloading_prompt_reports_the_budget_fitted_terms_it_uses() {
    let _lock = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dictionary.json");
    std::fs::write(&path, r#"{"terms":["Factus","Codex"]}"#).unwrap();
    let _env = EnvGuard::dictionary(&path);

    let (prompt, terms) = ReloadingDictionary::new(ReloadPrecedence::EnvFirst)
        .initial_prompt_with_terms(Some("base"));

    assert_eq!(prompt.as_deref(), Some("base\nVocabulary: Factus"));
    assert_eq!(terms, ["Factus"]);
}

#[test]
fn owned_dictionary_settings_reload_terms_and_replacements_without_process_env() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.json");
    let second = dir.path().join("second.json");
    std::fs::write(
        &first,
        r#"{"terms":["Factus"],"replacements":{"cloud code":"Claude Code"}}"#,
    )
    .unwrap();
    std::fs::write(
        &second,
        r#"{"terms":["Codex"],"replacements":{"code x":"Codex"}}"#,
    )
    .unwrap();

    let mut dictionary = ReloadingDictionary::from_settings(RuntimeDictionarySettings::new(
        true,
        vec![first],
        10,
        1200,
    ));
    assert_eq!(dictionary.initial_prompt_with_terms(None).1, ["Factus"]);
    assert_eq!(
        dictionary
            .current()
            .apply_replacements("cloud code")
            .unwrap()
            .0,
        "Claude Code"
    );

    dictionary.apply_settings(&std::collections::BTreeMap::from([
        ("dictionary".to_owned(), second.display().to_string()),
        ("dictionary_enabled".to_owned(), "1".to_owned()),
        ("dictionary_max_terms".to_owned(), "10".to_owned()),
        ("dictionary_prompt_chars".to_owned(), "1200".to_owned()),
    ]));
    assert_eq!(dictionary.initial_prompt_with_terms(None).1, ["Codex"]);
    assert_eq!(
        dictionary.current().apply_replacements("code x").unwrap().0,
        "Codex"
    );
}
