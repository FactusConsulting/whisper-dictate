use crate::cli::{Cli, Command, HistoryCommand};
use clap::Parser;

#[test]
fn parses_history_list_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "history", "list", "25"]);
    assert_eq!(
        cli.command,
        Some(Command::History {
            command: HistoryCommand::List { limit: 25 },
        })
    );
}

#[test]
fn parses_history_last_defaults_n_1_no_json() {
    // Bare `history last` must still parse — the flag additions must
    // remain backward-compatible with the shipping invocation.
    let cli = Cli::parse_from(["whisper-dictate", "history", "last"]);
    assert_eq!(
        cli.command,
        Some(Command::History {
            command: HistoryCommand::Last { n: 1, json: false },
        })
    );
}

#[test]
fn parses_history_last_with_flags() {
    let cli = Cli::parse_from(["whisper-dictate", "history", "last", "--n", "5", "--json"]);
    assert_eq!(
        cli.command,
        Some(Command::History {
            command: HistoryCommand::Last { n: 5, json: true },
        })
    );
}

#[test]
fn parses_history_copy_last() {
    let cli = Cli::parse_from(["whisper-dictate", "history", "copy-last"]);
    assert_eq!(
        cli.command,
        Some(Command::History {
            command: HistoryCommand::CopyLast,
        })
    );
}

#[test]
fn parses_history_reinject_last_defaults() {
    // Bare `history reinject-last` MUST default to safe (dry_run=false,
    // do_it=false — handler treats as dry-run).
    let cli = Cli::parse_from(["whisper-dictate", "history", "reinject-last"]);
    assert_eq!(
        cli.command,
        Some(Command::History {
            command: HistoryCommand::ReinjectLast {
                dry_run: false,
                do_it: false,
                json: false,
                backend: "auto".to_owned(),
            },
        })
    );
}

#[test]
fn parses_history_reinject_last_do_it_json() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "history",
        "reinject-last",
        "--do-it",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::History {
            command: HistoryCommand::ReinjectLast {
                dry_run: false,
                do_it: true,
                json: true,
                backend: "auto".to_owned(),
            },
        })
    );
}

#[test]
fn parses_history_search_with_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "history",
        "search",
        "codex",
        "--limit",
        "5",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::History {
            command: HistoryCommand::Search {
                query: "codex".to_owned(),
                limit: 5,
                json: true,
            },
        })
    );
}

#[test]
fn parses_legacy_hidden_inject_text_helper_flags() {
    // The internal `inject-text --mode type --text ... --xkb-layout ...`
    // form must keep parsing exactly as before even though the command
    // also has public-form flags.
    let cli = Cli::parse_from([
        "whisper-dictate",
        "inject-text",
        "--mode",
        "type",
        "--text",
        "høre",
        "--xkb-layout",
        "dk",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::InjectText {
            text_arg: None,
            dry_run: false,
            do_it: false,
            backend: "auto".to_owned(),
            json: false,
            mode: "type".to_owned(),
            text: "høre".to_owned(),
            xkb_layout: "dk".to_owned(),
            target_title: String::new(),
            target_process: String::new(),
        })
    );
}

#[test]
fn parses_public_inject_text_positional_defaults_to_dry_run() {
    // Public form: positional TEXT + implicit dry-run (no --do-it).
    let cli = Cli::parse_from(["whisper-dictate", "inject-text", "smoke test"]);
    assert_eq!(
        cli.command,
        Some(Command::InjectText {
            text_arg: Some("smoke test".to_owned()),
            dry_run: false, // flag not passed; handler still treats as dry-run
            do_it: false,
            backend: "auto".to_owned(),
            json: false,
            mode: String::new(),
            text: String::new(),
            xkb_layout: String::new(),
            target_title: String::new(),
            target_process: String::new(),
        })
    );
}

#[test]
fn parses_public_inject_text_with_explicit_dry_run_and_json_and_backend() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "inject-text",
        "hej",
        "--dry-run",
        "--backend",
        "wtype",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::InjectText {
            text_arg: Some("hej".to_owned()),
            dry_run: true,
            do_it: false,
            backend: "wtype".to_owned(),
            json: true,
            mode: String::new(),
            text: String::new(),
            xkb_layout: String::new(),
            target_title: String::new(),
            target_process: String::new(),
        })
    );
}

#[test]
fn parses_public_inject_text_do_it_and_live_alias() {
    // `--do-it` opts into real injection.
    let cli = Cli::parse_from(["whisper-dictate", "inject-text", "hi", "--do-it"]);
    assert!(matches!(
        cli.command,
        Some(Command::InjectText { do_it: true, .. })
    ));
    // `--live` is an alias for the same flag — same parsed state.
    let cli = Cli::parse_from(["whisper-dictate", "inject-text", "hi", "--live"]);
    assert!(matches!(
        cli.command,
        Some(Command::InjectText { do_it: true, .. })
    ));
}
