use crate::cli::{Cli, Command, ModelsCommand};
use clap::Parser;

#[test]
fn parses_bench_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "bench"]);
    assert_eq!(cli.command, Some(Command::Bench));
}

#[test]
fn parses_benchmark_alias_subcommand() {
    // `benchmark` is exposed as an alias of `bench` so users (and the
    // older docs) can spell it out without the parser tripping.
    let cli = Cli::parse_from(["whisper-dictate", "benchmark"]);
    assert_eq!(cli.command, Some(Command::Bench));
}

#[test]
fn parses_corpus_record_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "corpus-record", "da-001"]);
    assert_eq!(
        cli.command,
        Some(Command::CorpusRecord {
            id: "da-001".to_owned(),
        })
    );
}

#[test]
fn parses_models_list_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "models", "list"]);
    assert_eq!(
        cli.command,
        Some(Command::Models {
            command: ModelsCommand::List,
        })
    );
}

#[test]
fn parses_models_download_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "models", "download", "tiny.en"]);
    assert_eq!(
        cli.command,
        Some(Command::Models {
            command: ModelsCommand::Download {
                name: "tiny.en".to_owned(),
            },
        })
    );
}

#[test]
fn parses_models_path_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "models", "path"]);
    assert_eq!(
        cli.command,
        Some(Command::Models {
            command: ModelsCommand::Path,
        })
    );
}
