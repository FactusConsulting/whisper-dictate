use crate::cli::{Cli, Command, DictionaryCommand};
use clap::Parser;

#[test]
fn parses_dictionary_add_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "dictionary", "add", "Codex"]);
    assert_eq!(
        cli.command,
        Some(Command::Dictionary {
            command: DictionaryCommand::Add {
                term: "Codex".to_owned(),
            },
        })
    );
}

#[test]
fn parses_dictionary_prompt_defaults() {
    let cli = Cli::parse_from(["whisper-dictate", "dictionary", "prompt"]);
    assert_eq!(
        cli.command,
        Some(Command::Dictionary {
            command: DictionaryCommand::Prompt {
                dictionary: None,
                json: false,
                max_length: None,
            },
        })
    );
}

#[test]
fn parses_dictionary_prompt_with_all_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "dictionary",
        "prompt",
        "--dictionary",
        "d.json",
        "--json",
        "--max-length",
        "256",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::Dictionary {
            command: DictionaryCommand::Prompt {
                dictionary: Some("d.json".to_owned()),
                json: true,
                max_length: Some(256),
            },
        })
    );
}

#[test]
fn parses_dictionary_list_defaults() {
    let cli = Cli::parse_from(["whisper-dictate", "dictionary", "list"]);
    assert_eq!(
        cli.command,
        Some(Command::Dictionary {
            command: DictionaryCommand::List {
                dictionary: None,
                json: false,
            },
        })
    );
}

#[test]
fn parses_dictionary_list_with_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "dictionary",
        "list",
        "--dictionary",
        "d.json",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::Dictionary {
            command: DictionaryCommand::List {
                dictionary: Some("d.json".to_owned()),
                json: true,
            },
        })
    );
}

#[test]
fn parses_dictionary_build_from_corpus_with_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "dictionary",
        "build-from-corpus",
        "--benchmark-corpus",
        "corpus.json",
        "--language",
        "da",
        "--category",
        "technical",
        "--dictionary",
        "d.json",
        "--apply",
        "--min-count",
        "2",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::Dictionary {
            command: DictionaryCommand::BuildFromCorpus {
                benchmark_corpus: Some("corpus.json".to_owned()),
                app_root: None,
                dictionary: Some("d.json".to_owned()),
                language: Some("da".to_owned()),
                category: Some("technical".to_owned()),
                min_count: 2,
                apply: true,
                json: true,
            },
        })
    );
}

#[test]
fn parses_dictionary_build_from_corpus_with_defaults() {
    // No flags besides the subcommand: every option uses its native
    // default (no corpus override, no dict override, min_count=1,
    // preview-only, no JSON).
    let cli = Cli::parse_from(["whisper-dictate", "dictionary", "build-from-corpus"]);
    assert_eq!(
        cli.command,
        Some(Command::Dictionary {
            command: DictionaryCommand::BuildFromCorpus {
                benchmark_corpus: None,
                app_root: None,
                dictionary: None,
                language: None,
                category: None,
                min_count: 1,
                apply: false,
                json: false,
            },
        })
    );
}

#[test]
fn parses_dictionary_suggest_terms_with_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "dictionary",
        "suggest-terms",
        "results.jsonl",
        "--dictionary",
        "d.json",
        "--apply",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::Dictionary {
            command: DictionaryCommand::SuggestTerms {
                jsonl: "results.jsonl".to_owned(),
                dictionary: Some("d.json".to_owned()),
                min_count: 1,
                apply: true,
                json: true,
            },
        })
    );
}

#[test]
fn parses_hidden_dictionary_runtime_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "dictionary-runtime"]);
    assert_eq!(cli.command, Some(Command::DictionaryRuntime));
}

#[test]
fn parses_hidden_dictate_ops_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "dictate-ops"]);
    assert_eq!(cli.command, Some(Command::DictateOps));
}
