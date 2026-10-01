use crate::cli::{Cli, Command, HotkeyCommand};
use clap::Parser;

#[test]
fn parses_hotkey_capture_default_flags() {
    let cli = Cli::parse_from(["whisper-dictate", "hotkey", "capture"]);
    assert_eq!(
        cli.command,
        Some(Command::Hotkey {
            command: HotkeyCommand::Capture {
                for_secs: "5".to_owned(),
                json: false,
                chord_events_only: false,
                focus_process: None,
                exit_on_chord: false,
                configure: false,
                config: None,
                // Default driver is `auto` — the shipping selection logic
                // (evdev on Linux Wayland, rdev everywhere else). Pin
                // the default here so a CLI-schema change is caught.
                driver: "auto".to_owned(),
                // No override -- the chord comes from the config.
                chord: None,
            },
        })
    );
}

#[test]
fn parses_hotkey_capture_all_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "hotkey",
        "capture",
        "--for",
        "0.5",
        "--json",
        "--chord-events-only",
        "--focus-process",
        "4242",
        "--exit-on-chord",
        "--config",
        "/tmp/cfg.json",
        "--driver",
        "evdev",
        "--chord",
        "shift_r+f9",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::Hotkey {
            command: HotkeyCommand::Capture {
                for_secs: "0.5".to_owned(),
                json: true,
                chord_events_only: true,
                focus_process: Some(4242),
                exit_on_chord: true,
                configure: false,
                config: Some("/tmp/cfg.json".to_owned()),
                driver: "evdev".to_owned(),
                chord: Some("shift_r+f9".to_owned()),
            },
        })
    );
}

#[test]
fn parses_hotkey_capture_chord_override_alone() {
    // `--chord` must work without `--config`: the whole point is to test
    // a chord WITHOUT touching the user's saved settings.
    let cli = Cli::parse_from(["whisper-dictate", "hotkey", "capture", "--chord", "ctrl_l"]);
    match cli.command {
        Some(Command::Hotkey {
            command: HotkeyCommand::Capture { chord, config, .. },
        }) => {
            assert_eq!(chord, Some("ctrl_l".to_owned()));
            assert_eq!(config, None);
        }
        other => panic!("expected hotkey capture with --chord, got {other:?}"),
    }
}

#[test]
fn parses_hotkey_capture_configure_flag() {
    let cli = Cli::parse_from(["whisper-dictate", "hotkey", "capture", "--configure"]);
    match cli.command {
        Some(Command::Hotkey {
            command: HotkeyCommand::Capture { configure, .. },
        }) => assert!(configure),
        other => panic!("expected hotkey capture configure flag, got {other:?}"),
    }
}

#[test]
fn parses_hotkey_capture_driver_flag_rdev() {
    // `--driver rdev` must be accepted as an explicit override so the
    // smoke script can pin the X11 backend on a mixed session too.
    let cli = Cli::parse_from([
        "whisper-dictate",
        "hotkey",
        "capture",
        "--for",
        "0.1",
        "--driver",
        "rdev",
    ]);
    match cli.command {
        Some(Command::Hotkey {
            command: HotkeyCommand::Capture { driver, .. },
        }) => assert_eq!(driver, "rdev"),
        other => panic!("expected hotkey capture with --driver rdev, got {other:?}"),
    }
}
