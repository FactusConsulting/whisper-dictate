use crate::cli::{Cli, Command};
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
