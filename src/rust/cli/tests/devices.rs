use crate::cli::{Cli, Command, DevicesCommand};
use clap::Parser;

#[test]
fn parses_devices_subcommand() {
    // Bare `devices` parses the internal JSON-envelope form.
    let cli = Cli::parse_from(["whisper-dictate", "devices"]);
    assert_eq!(cli.command, Some(Command::Devices { command: None }));
}

#[test]
fn parses_devices_test_subcommand() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "devices",
        "test",
        "Microphone (Yeti Classic)",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::Devices {
            command: Some(DevicesCommand::Test {
                name: "Microphone (Yeti Classic)".to_owned(),
            }),
        })
    );
}

#[test]
fn parses_devices_test_empty_name_for_system_default() {
    // Empty string is the documented way to test the system default input.
    let cli = Cli::parse_from(["whisper-dictate", "devices", "test", ""]);
    assert_eq!(
        cli.command,
        Some(Command::Devices {
            command: Some(DevicesCommand::Test {
                name: String::new(),
            }),
        })
    );
}

#[test]
fn parses_devices_test_with_hyphen_leading_name() {
    // Rare hardware names can start with `-`; clap must let it through so
    // the device resolver decides "not found" rather than clap rejecting
    // it as a stray flag. Matches the corpus-record hyphen precedent.
    let cli = Cli::parse_from(["whisper-dictate", "devices", "test", "-hyphen-mic"]);
    assert_eq!(
        cli.command,
        Some(Command::Devices {
            command: Some(DevicesCommand::Test {
                name: "-hyphen-mic".to_owned(),
            }),
        })
    );
}
