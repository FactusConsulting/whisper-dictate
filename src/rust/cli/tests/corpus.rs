use crate::cli::{Cli, Command};
use clap::Parser;

#[test]
fn corpus_record_accepts_hyphen_leading_id() {
    // A manifest item whose
    // safe id starts with `-` (e.g. `-sample`) must reach the validator
    // rather than getting rejected by clap as a stray flag. The full
    // `[A-Za-z0-9._-]+` allowlist is then enforced by
    // `corpus_record::is_safe_corpus_id` BEFORE shelling out.
    let cli = Cli::parse_from(["whisper-dictate", "corpus-record", "-sample"]);
    assert_eq!(
        cli.command,
        Some(Command::CorpusRecord {
            id: "-sample".to_owned(),
        })
    );
}
