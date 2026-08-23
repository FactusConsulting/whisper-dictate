use super::runtime::config_dictionary_paths;
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

    fn configured_clear(
        config: &std::path::Path,
        ambient_dictionary: &std::path::Path,
        config_root: &std::path::Path,
    ) -> Self {
        let keys = [
            "VOICEPI_CONFIG",
            "VOICEPI_DICTIONARY",
            "VOICEPI_DICTIONARY_ENABLED",
            "VOICEPI_DICTIONARY_MAX_TERMS",
            "VOICEPI_DICTIONARY_PROMPT_CHARS",
            "APPDATA",
            "XDG_CONFIG_HOME",
            "HOME",
        ];
        let saved = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        std::env::set_var("VOICEPI_CONFIG", config);
        std::env::set_var("VOICEPI_DICTIONARY", ambient_dictionary);
        std::env::remove_var("VOICEPI_DICTIONARY_ENABLED");
        std::env::remove_var("VOICEPI_DICTIONARY_MAX_TERMS");
        std::env::remove_var("VOICEPI_DICTIONARY_PROMPT_CHARS");
        std::env::set_var("APPDATA", config_root);
        std::env::set_var("XDG_CONFIG_HOME", config_root);
        std::env::set_var("HOME", config_root);
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

#[test]
fn explicit_dictionary_null_suppresses_ambient_terms_and_replacements() {
    let _lock = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let dictionary_path = dir.path().join("ambient-dictionary.json");
    std::fs::write(&config_path, r#"{"dictionary":null}"#).unwrap();
    std::fs::write(
        &dictionary_path,
        r#"{"terms":["Factus"],"replacements":{"cloud code":"Claude Code"}}"#,
    )
    .unwrap();
    let old_config = std::env::var_os("VOICEPI_CONFIG");
    let old_dictionary = std::env::var_os("VOICEPI_DICTIONARY");
    std::env::set_var("VOICEPI_CONFIG", &config_path);
    std::env::set_var("VOICEPI_DICTIONARY", &dictionary_path);

    let configured = crate::config::load_settings().unwrap();
    assert!(configured.dictionary.is_empty());
    let mut dictionary = ReloadingDictionary::from_settings(RuntimeDictionarySettings::new(
        configured.dictionary_enabled,
        config_dictionary_paths(&configured),
        configured.dictionary_max_terms.parse().unwrap(),
        configured.dictionary_prompt_chars.parse().unwrap(),
    ));

    assert!(dictionary.initial_prompt_with_terms(None).1.is_empty());
    assert_eq!(
        dictionary
            .current()
            .apply_replacements("cloud code")
            .unwrap()
            .0,
        "cloud code"
    );
    crate::config::test_support::restore_env("VOICEPI_CONFIG", old_config);
    crate::config::test_support::restore_env("VOICEPI_DICTIONARY", old_dictionary);
}

#[test]
fn dictionary_management_clear_avoids_ambient_term_and_replacement_mutations() {
    let _lock = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let ambient_path = dir.path().join("ambient-dictionary.json");
    let config_root = dir.path().join("config-root");
    std::fs::write(&config_path, r#"{"dictionary":null}"#).unwrap();
    let ambient_original =
        r#"{"terms":["Ambient"],"replacements":{"ambient word":"Ambient Word"}}"#;
    std::fs::write(&ambient_path, ambient_original).unwrap();
    let _env = EnvGuard::configured_clear(&config_path, &ambient_path, &config_root);
    let default_path = super::default_dictionary_path();

    super::handle_command(crate::cli::DictionaryCommand::Add {
        term: "LocalTerm".to_owned(),
    })
    .unwrap();
    super::handle_command(crate::cli::DictionaryCommand::Replace {
        mapping: "local word=Local Word".to_owned(),
    })
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(&ambient_path).unwrap(),
        ambient_original
    );
    let managed = super::load_dictionary(&default_path).unwrap();
    assert_eq!(managed.terms, ["LocalTerm"]);
    assert!(managed
        .replacements
        .iter()
        .any(|replacement| replacement.from == "local word" && replacement.to == "Local Word"));
}
