use crate::cli::{Cli, Command, ConfigCommand};
use clap::Parser;

#[test]
fn parses_config_path_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "config", "path"]);
    assert_eq!(
        cli.command,
        Some(Command::Config {
            command: ConfigCommand::Path,
        })
    );
}

#[test]
fn parses_config_get_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "config", "get", "audio_device"]);
    assert_eq!(
        cli.command,
        Some(Command::Config {
            command: ConfigCommand::Get {
                key: "audio_device".to_owned(),
                json: false,
                config: None,
            },
        })
    );

    let cli = Cli::parse_from([
        "whisper-dictate",
        "config",
        "get",
        "model",
        "--json",
        "--config",
        "/tmp/cfg.json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::Config {
            command: ConfigCommand::Get {
                key: "model".to_owned(),
                json: true,
                config: Some("/tmp/cfg.json".to_owned()),
            },
        })
    );
}

#[test]
fn parses_config_set_subcommand() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "config",
        "set",
        "model",
        "large-v3-turbo",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::Config {
            command: ConfigCommand::Set {
                key: "model".to_owned(),
                value: "large-v3-turbo".to_owned(),
                config: None,
            },
        })
    );
}

#[test]
fn parses_config_set_accepts_hyphen_leading_value() {
    // A device name or a negative dBFS may start with `-`; clap must let
    // it through so the settings validator (not clap) is the one that
    // sees the raw value.
    let cli = Cli::parse_from(["whisper-dictate", "config", "set", "target_dbfs", "-24"]);
    assert_eq!(
        cli.command,
        Some(Command::Config {
            command: ConfigCommand::Set {
                key: "target_dbfs".to_owned(),
                value: "-24".to_owned(),
                config: None,
            },
        })
    );
}

#[test]
fn parses_config_list_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "config", "list"]);
    assert_eq!(
        cli.command,
        Some(Command::Config {
            command: ConfigCommand::List {
                json: false,
                config: None,
            },
        })
    );

    let cli = Cli::parse_from(["whisper-dictate", "config", "list", "--json"]);
    assert_eq!(
        cli.command,
        Some(Command::Config {
            command: ConfigCommand::List {
                json: true,
                config: None,
            },
        })
    );
}
