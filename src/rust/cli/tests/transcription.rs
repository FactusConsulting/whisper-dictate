use crate::cli::{Cli, Command, DevicesCommand};
use clap::Parser;

#[test]
fn rejects_unknown_backend_at_parse_time() {
    // The value_parser allowlist stops typos before the handler runs.
    let err = Cli::try_parse_from([
        "whisper-dictate",
        "inject-text",
        "hi",
        "--backend",
        "notabackend",
    ]);
    assert!(err.is_err(), "expected clap to reject unknown backend");
}

#[test]
fn parses_hidden_format_text_subcommand() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "format-text",
        "--text",
        "første komma",
        "--command-set",
        "da",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::FormatText {
            text: "første komma".to_owned(),
            command_set: "da".to_owned(),
        })
    );
}

#[test]
fn parses_hidden_cloud_transcribe_subcommand() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "cloud-transcribe",
        "--base-url",
        "https://api.openai.com/v1",
        "--api-key",
        "key",
        "--model",
        "gpt-4o-mini-transcribe",
        "--audio-wav-path",
        "audio.wav",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::CloudTranscribe {
            base_url: "https://api.openai.com/v1".to_owned(),
            api_key: "key".to_owned(),
            model: "gpt-4o-mini-transcribe".to_owned(),
            audio_wav_path: "audio.wav".to_owned(),
            language: String::new(),
            prompt: String::new(),
            timeout_ms: 30000,
        })
    );
}

#[test]
fn parses_public_transcribe_file_subcommand() {
    let plain = Cli::parse_from(["whisper-dictate", "transcribe-file", "recording.wav"]);
    assert_eq!(
        plain.command,
        Some(Command::TranscribeFile {
            path: "recording.wav".to_owned(),
            json: false,
        })
    );

    let json = Cli::parse_from([
        "whisper-dictate",
        "transcribe-file",
        "recording.wav",
        "--json",
    ]);
    assert_eq!(
        json.command,
        Some(Command::TranscribeFile {
            path: "recording.wav".to_owned(),
            json: true,
        })
    );
}

#[test]
fn parses_native_calibration_commands() {
    let mic = Cli::parse_from([
        "whisper-dictate",
        "calibrate-mic",
        "4.5",
        "--device",
        "USB Mic",
        "--json",
    ]);
    assert_eq!(
        mic.command,
        Some(Command::CalibrateMic {
            seconds: 4.5,
            device: Some("USB Mic".to_owned()),
            json: true,
        })
    );

    let file = Cli::parse_from(["whisper-dictate", "calibrate-file", "sample.wav", "--json"]);
    assert_eq!(
        file.command,
        Some(Command::CalibrateFile {
            path: "sample.wav".to_owned(),
            json: true,
        })
    );
}

#[test]
fn transcribe_file_requires_exactly_one_path() {
    assert!(Cli::try_parse_from(["whisper-dictate", "transcribe-file"]).is_err());
    assert!(
        Cli::try_parse_from(["whisper-dictate", "transcribe-file", "one.wav", "two.wav",]).is_err()
    );
}

#[test]
fn parses_hidden_telemetry_helpers() {
    let cli = Cli::parse_from(["whisper-dictate", "append-jsonl", "--path", "metrics.jsonl"]);
    assert_eq!(
        cli.command,
        Some(Command::AppendJsonl {
            path: "metrics.jsonl".to_owned(),
        })
    );

    let cli = Cli::parse_from([
        "whisper-dictate",
        "append-history",
        "--path",
        "history.jsonl",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::AppendHistory {
            path: "history.jsonl".to_owned(),
        })
    );

    let cli = Cli::parse_from(["whisper-dictate", "append-record-sinks"]);
    assert_eq!(cli.command, Some(Command::AppendRecordSinks));

    let cli = Cli::parse_from(["whisper-dictate", "worker-event"]);
    assert_eq!(cli.command, Some(Command::WorkerEvent));

    let cli = Cli::parse_from(["whisper-dictate", "command-hook"]);
    assert_eq!(cli.command, Some(Command::CommandHook));

    let cli = Cli::parse_from(["whisper-dictate", "redact-text"]);
    assert_eq!(cli.command, Some(Command::RedactText));

    let cli = Cli::parse_from(["whisper-dictate", "apply-profile"]);
    assert_eq!(cli.command, Some(Command::ApplyProfile));

    let cli = Cli::parse_from(["whisper-dictate", "privacy"]);
    assert_eq!(cli.command, Some(Command::Privacy));

    let cli = Cli::parse_from(["whisper-dictate", "postprocess"]);
    assert_eq!(cli.command, Some(Command::Postprocess));

    let cli = Cli::parse_from(["whisper-dictate", "external-api"]);
    assert_eq!(cli.command, Some(Command::ExternalApi));

    let cli = Cli::parse_from(["whisper-dictate", "health"]);
    assert_eq!(cli.command, Some(Command::Health));

    let cli = Cli::parse_from(["whisper-dictate", "transcribe-wav"]);
    assert_eq!(cli.command, Some(Command::TranscribeWav { probe: false }));

    let cli = Cli::parse_from(["whisper-dictate", "transcribe-wav", "--probe"]);
    assert_eq!(cli.command, Some(Command::TranscribeWav { probe: true }));

    // Long-running in-process worker subcommand.
    let cli = Cli::parse_from(["whisper-dictate", "transcribe-server"]);
    assert_eq!(cli.command, Some(Command::TranscribeServer));
}

#[test]
fn parses_hidden_inject_subcommand() {
    let cli = Cli::parse_from(["whisper-dictate", "inject"]);
    assert_eq!(cli.command, Some(Command::Inject));
}

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
