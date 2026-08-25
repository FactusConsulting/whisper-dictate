use std::path::PathBuf;

use super::super::runtime_settings::RuntimeDictionarySettings;
use super::super::{Replacement, ReplacementChange};
use super::runtime_dictionary_result;

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
