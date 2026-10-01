use crate::cli::{Cli, Command, SelfTestCommand};
use clap::Parser;

#[test]
fn parses_self_test_ptt_wedge_default_flags() {
    // Defaults: 5 iterations, plain output, auto driver. Pin the shape so
    // a CLI-schema change is caught before the smoke script breaks.
    let cli = Cli::parse_from(["whisper-dictate", "self-test", "ptt-wedge"]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::PttWedge {
                iterations: 5,
                json: false,
                driver: "auto".to_owned(),
            },
        })
    );
}

#[test]
fn parses_self_test_ptt_wedge_all_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "ptt-wedge",
        "--iterations",
        "3",
        "--json",
        "--driver",
        "evdev",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::PttWedge {
                iterations: 3,
                json: true,
                driver: "evdev".to_owned(),
            },
        })
    );
}

#[test]
fn parses_self_test_ptt_wedge_rdev_driver() {
    // Explicit rdev override for smoke-script pinning on X11 boxes.
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "ptt-wedge",
        "--driver",
        "rdev",
    ]);
    match cli.command {
        Some(Command::SelfTest {
            command: SelfTestCommand::PttWedge { driver, .. },
        }) => assert_eq!(driver, "rdev"),
        other => panic!("expected self-test ptt-wedge --driver rdev, got {other:?}"),
    }
}

// -----------------------------------------------------------------------
// self-test injection-idempotency — sibling to ptt-wedge, different
// bug class (injection-side state accumulation).
// -----------------------------------------------------------------------

#[test]
fn parses_self_test_injection_idempotency_default_flags() {
    // Defaults: 10 iterations, plain output, auto backend, dry-run
    // (live=false). Pin the shape so a CLI-schema change is caught
    // before the smoke script breaks.
    let cli = Cli::parse_from(["whisper-dictate", "self-test", "injection-idempotency"]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::InjectionIdempotency {
                iterations: 10,
                json: false,
                backend: "auto".to_owned(),
                live: false,
            },
        })
    );
}

#[test]
fn parses_self_test_injection_idempotency_all_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "injection-idempotency",
        "--iterations",
        "3",
        "--json",
        "--backend",
        "enigo",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::InjectionIdempotency {
                iterations: 3,
                json: true,
                backend: "enigo".to_owned(),
                live: false,
            },
        })
    );
}

#[test]
fn parses_self_test_injection_idempotency_live_flag() {
    // `--live` opts into real injection — must parse but never
    // default to true. The handler treats live=true as the dangerous
    // path and prints a warning before executing.
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "injection-idempotency",
        "--live",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::InjectionIdempotency {
                iterations: 10,
                json: false,
                backend: "auto".to_owned(),
                live: true,
            },
        })
    );
}

#[test]
fn rejects_unknown_backend_for_injection_idempotency() {
    // The value_parser allowlist stops typos at parse time so the
    // handler never has to defend against them — mirrors the
    // `inject-text --backend` guardrail.
    let err = Cli::try_parse_from([
        "whisper-dictate",
        "self-test",
        "injection-idempotency",
        "--backend",
        "notabackend",
    ]);
    assert!(err.is_err(), "expected clap to reject unknown backend");
}

// self-test audio-capture — native cpal capture and resampling.
// Wires the cpal capture path into a headless self-test that also
// applies the configured PipeWire quantum before opening the
// stream. The CLI-shape tests here run on every build so the smoke
// script's pinned flags survive an accidental rename.

#[test]
fn parses_self_test_audio_capture_default_flags() {
    let cli = Cli::parse_from(["whisper-dictate", "self-test", "audio-capture"]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::AudioCapture {
                duration_ms: 2000,
                device: String::new(),
                json: false,
                fail_on_silence: false,
            },
        })
    );
}

#[test]
fn parses_self_test_audio_capture_all_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "audio-capture",
        "--duration-ms",
        "500",
        "--device",
        "Yeti Classic",
        "--json",
        "--fail-on-silence",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::AudioCapture {
                duration_ms: 500,
                device: "Yeti Classic".to_owned(),
                json: true,
                fail_on_silence: true,
            },
        })
    );
}

// -----------------------------------------------------------------------
// dictate-run — audit item 5 Phase A step 1
// -----------------------------------------------------------------------

#[test]
fn parses_dictate_run_subcommand_with_defaults() {
    // Bare `dictate-run` must parse so callers can invoke it without
    // arguments and get the resolved-config behaviour.
    let cli = Cli::parse_from(["whisper-dictate", "dictate-run"]);
    assert_eq!(
        cli.command,
        Some(Command::DictateRun {
            config: None,
            json_events: false,
            foreground: false,
        })
    );
}

#[test]
fn parses_dictate_run_subcommand_with_all_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "dictate-run",
        "--config",
        "/tmp/wd.json",
        "--json-events",
        "--foreground",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::DictateRun {
            config: Some("/tmp/wd.json".to_owned()),
            json_events: true,
            foreground: true,
        })
    );
}

#[test]
fn parses_dictate_run_json_events_alone() {
    // Pin `--json-events` on its own so a clap-derive refactor cannot
    // accidentally coalesce the flags.
    let cli = Cli::parse_from(["whisper-dictate", "dictate-run", "--json-events"]);
    assert_eq!(
        cli.command,
        Some(Command::DictateRun {
            config: None,
            json_events: true,
            foreground: false,
        })
    );
}

// -----------------------------------------------------------------------
// self-test round 2/3 backend verbs — feedback, audio-ducking,
// profile-match, history-write, metrics-write, preview. CLI-shape tests
// pin the flag names + defaults so the smoke script (which invokes each
// verb with a fixed argv) cannot drift silently.
// -----------------------------------------------------------------------

#[test]
fn parses_self_test_feedback_default_flags() {
    let cli = Cli::parse_from(["whisper-dictate", "self-test", "feedback"]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::Feedback {
                delay_ms: 100,
                json: false,
            },
        })
    );
}

#[test]
fn parses_self_test_feedback_all_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "feedback",
        "--delay-ms",
        "0",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::Feedback {
                delay_ms: 0,
                json: true,
            },
        })
    );
}

#[test]
fn parses_self_test_audio_ducking_default_flags() {
    let cli = Cli::parse_from(["whisper-dictate", "self-test", "audio-ducking"]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::AudioDucking {
                duration_ms: 500,
                json: false,
            },
        })
    );
}

#[test]
fn parses_self_test_audio_ducking_with_short_duration_and_json() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "audio-ducking",
        "--duration-ms",
        "200",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::AudioDucking {
                duration_ms: 200,
                json: true,
            },
        })
    );
}

#[test]
fn parses_self_test_profile_match_default_flags() {
    let cli = Cli::parse_from(["whisper-dictate", "self-test", "profile-match"]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::ProfileMatch {
                title: String::new(),
                process: String::new(),
                json: false,
            },
        })
    );
}

#[test]
fn parses_self_test_profile_match_with_synthetic_window() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "profile-match",
        "--title",
        "Cursor",
        "--process",
        "cursor.exe",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::ProfileMatch {
                title: "Cursor".to_owned(),
                process: "cursor.exe".to_owned(),
                json: true,
            },
        })
    );
}

#[test]
fn parses_self_test_history_write_default_flags() {
    let cli = Cli::parse_from(["whisper-dictate", "self-test", "history-write"]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::HistoryWrite {
                text: String::new(),
                json: false,
            },
        })
    );
}

#[test]
fn parses_self_test_history_write_with_text_and_json() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "history-write",
        "--text",
        "smoke test",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::HistoryWrite {
                text: "smoke test".to_owned(),
                json: true,
            },
        })
    );
}

#[test]
fn parses_self_test_metrics_write_default_flags() {
    let cli = Cli::parse_from(["whisper-dictate", "self-test", "metrics-write"]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::MetricsWrite {
                text: String::new(),
                json: false,
            },
        })
    );
}

#[test]
fn parses_self_test_metrics_write_with_text_and_json() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "metrics-write",
        "--text",
        "smoke test",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::MetricsWrite {
                text: "smoke test".to_owned(),
                json: true,
            },
        })
    );
}

#[test]
fn parses_self_test_preview_default_flags() {
    let cli = Cli::parse_from(["whisper-dictate", "self-test", "preview"]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::Preview {
                frames: 5,
                frame_samples: 24_000,
                sample_rate: 16_000,
                interval_ms: 100,
                canned_text: "canned preview".to_owned(),
                json: false,
            },
        })
    );
}

#[test]
fn parses_self_test_preview_all_flags() {
    let cli = Cli::parse_from([
        "whisper-dictate",
        "self-test",
        "preview",
        "--frames",
        "3",
        "--frame-samples",
        "16000",
        "--sample-rate",
        "16000",
        "--interval-ms",
        "50",
        "--canned-text",
        "hi",
        "--json",
    ]);
    assert_eq!(
        cli.command,
        Some(Command::SelfTest {
            command: SelfTestCommand::Preview {
                frames: 3,
                frame_samples: 16_000,
                sample_rate: 16_000,
                interval_ms: 50,
                canned_text: "hi".to_owned(),
                json: true,
            },
        })
    );
}
