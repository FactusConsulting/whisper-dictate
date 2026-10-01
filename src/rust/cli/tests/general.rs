use crate::cli::{Cli, Command, ConfigCommand};
use clap::Parser;

#[test]
fn no_subcommand_opens_ui_by_default() {
    let cli = Cli::parse_from(["whisper-dictate"]);
    assert!(!cli.version);
    assert_eq!(cli.command, None);
}

#[test]
fn parses_version_flag() {
    let cli = Cli::parse_from(["whisper-dictate", "--version"]);
    assert!(cli.version);
    assert_eq!(cli.command, None);
}

#[test]
fn parses_run_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "run"]);
    assert_eq!(cli.command, Some(Command::Run { args: vec![] }));
}

#[test]
fn parses_list_windows_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "list-windows"]);
    assert_eq!(cli.command, Some(Command::ListWindows));
}

#[test]
fn parses_run_passthrough_args() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "run",
        "--key",
        "shift_r+ctrl_r",
        "--lang",
        "da",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::Run {
            args: vec![
                "--key".to_owned(),
                "shift_r+ctrl_r".to_owned(),
                "--lang".to_owned(),
                "da".to_owned(),
            ],
        })
    );
}

#[test]
fn parses_settings_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "settings"]);
    assert_eq!(cli.command, Some(Command::Settings));
}

#[test]
fn parses_doctor_subcommand_with_defaults() {
    let cli = Cli::parse_from(["whisper-dictate", "doctor"]);
    assert_eq!(
        cli.command,
        Some(Command::Doctor {
            json: false,
            config: None,
        })
    );
}

#[test]
fn parses_doctor_subcommand_with_json_and_config_override() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "doctor",
        "--json",
        "--config",
        "/tmp/broken.json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::Doctor {
            json: true,
            config: Some("/tmp/broken.json".to_owned()),
        })
    );
}

#[test]
fn parses_setup_ubuntu_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "setup-ubuntu"]);
    assert_eq!(cli.command, Some(Command::SetupUbuntu));
}

#[test]
fn parses_native_setup_and_export_subcommands() {
    let setup = Cli::parse_from(["whisper-dictate", "setup"]);
    assert_eq!(setup.command, Some(Command::Setup));

    let hidden = Cli::parse_from(["whisper-dictate", "export-config"]);
    assert_eq!(
        hidden.command,
        Some(Command::ExportConfig {
            include_secrets: false
        })
    );
    let shown = Cli::parse_from(["whisper-dictate", "export-config", "--include-secrets"]);
    assert_eq!(
        shown.command,
        Some(Command::ExportConfig {
            include_secrets: true
        })
    );
}

#[test]
fn parses_model_capacity_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "model-capacity"]);
    assert_eq!(cli.command, Some(Command::ModelCapacity { json: false }));

    let cli = Cli::parse_from(["whisper-dictate", "model-capacity", "--json"]);
    assert_eq!(cli.command, Some(Command::ModelCapacity { json: true }));
}

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
