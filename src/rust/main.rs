//! CLI entry point (`whisper-dictate.exe`). Console subsystem on every
//! platform — every CLI verb prints to stdout/stderr as expected when invoked
//! from PowerShell/cmd/a script. The tray UI lives in the sibling
//! `whisper-dictate-gui.exe` binary (windows-subsystem on Windows) so a
//! double-click from Explorer never flashes a cmd window. Both binaries
//! delegate to the shared library crate (`whisper_dictate_app`) — this file
//! is dispatch-only.

use std::process::ExitCode;

use clap::Parser;

use whisper_dictate_app::cli::{Cli, Command, DevicesCommand, SelfTestCommand};
use whisper_dictate_app::{
    benchmark, calibration, cloud_api, command_hook, config, corpus_record, dictate, dictionary,
    doctor, entrypoint, formatting, health, history, hotkey, injection, model_capacity,
    postprocess, privacy, profiles, redaction, runtime, telemetry, transcribe_file, ui, whisper,
};

fn main() -> ExitCode {
    // `_with_teardown`, never the bare shell: the finite rdev-driven verbs
    // (`self-test hotkey-boot`, `hotkey capture --for-secs ...`) queue
    // diagnostics on the async writer thread, and a bare return from `main`
    // would kill that thread with the tail of the capture still unwritten.
    // See `entrypoint::error_exit_shell_with_teardown`.
    entrypoint::error_exit_shell_with_teardown("error", std::io::stderr(), run)
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.version {
        println!("whisper-dictate {}", runtime::version());
        return Ok(());
    }

    match cli.command.unwrap_or(Command::Ui) {
        Command::Ui | Command::Settings => ui::run(),
        Command::Run { args } => runtime::run_terminal(args),
        Command::TranscribeFile { path, json } => {
            transcribe_file::handle(std::path::Path::new(&path), json)
        }
        Command::CalibrateMic {
            seconds,
            device,
            json,
        } => calibration::handle_microphone(seconds, device.as_deref(), json),
        Command::CalibrateFile { path, json } => {
            calibration::handle_file(std::path::Path::new(&path), json)
        }
        Command::Doctor { json, config } => doctor::handle_doctor(json, config.as_deref()),
        Command::Bench => benchmark::handle_bench(),
        Command::CorpusRecord { id } => corpus_record::handle_corpus_record(&id),
        Command::SimulateSession { wav, json, repeat } => {
            dictate::simulate::handle_simulate_session(&wav, json, repeat)
        }
        Command::DictateMic {
            device,
            seconds,
            json,
        } => handle_dictate_mic(&device, seconds, json),
        Command::Install => runtime::install(),
        Command::SetupUbuntu => runtime::setup_ubuntu(),
        Command::ModelCapacity { json } => model_capacity::handle_command(json),
        Command::Config { command } => config::handle_command(command),
        Command::Dictionary { command } => dictionary::handle_command(command),
        Command::DictionaryRuntime => dictionary::handle_runtime(),
        Command::DictateOps => dictate::ops::handle_ops(),
        Command::History { command } => history::handle_history_command(command),
        args @ Command::InjectText { .. } => dispatch_inject_text(args),
        Command::FormatText { text, command_set } => {
            formatting::handle_format_text(&text, &command_set)
        }
        Command::CloudTranscribe {
            base_url,
            api_key,
            model,
            audio_wav_path,
            language,
            prompt,
            timeout_ms,
        } => cloud_api::handle_cloud_transcribe(
            &base_url,
            &cloud_api::resolve_api_key(&api_key, &base_url),
            &model,
            audio_wav_path.as_ref(),
            (!language.trim().is_empty()).then_some(language.as_str()),
            (!prompt.trim().is_empty()).then_some(prompt.as_str()),
            timeout_ms,
        ),
        Command::AppendJsonl { path } => {
            telemetry::handle_append_jsonl(std::path::Path::new(&path))
        }
        Command::AppendHistory { path } => {
            telemetry::handle_append_history(std::path::Path::new(&path))
        }
        Command::AppendRecordSinks => telemetry::handle_append_record_sinks(),
        Command::WorkerEvent => telemetry::handle_worker_event(),
        Command::CommandHook => command_hook::handle_command_hook(),
        Command::RedactText => redaction::handle_redact_text(),
        Command::ApplyProfile => profiles::handle_apply_profile(),
        Command::Privacy => privacy::handle_privacy(),
        Command::Postprocess => postprocess::handle_postprocess(),
        Command::ExternalApi => cloud_api::handle_external_api(),
        Command::Health => health::handle_health(),
        Command::TranscribeWav { probe } => handle_transcribe_wav(probe),
        Command::TranscribeServer => handle_transcribe_server(),
        Command::Inject => injection::handle_inject(),
        Command::Devices { command } => match command {
            None => handle_devices_command(),
            Some(DevicesCommand::Test { name }) => handle_devices_test(&name),
        },
        Command::Models { command } => whisper::models_cli::handle(command),
        Command::Hotkey { command } => hotkey::capture::handle_hotkey_command(command),
        Command::SelfTest { command } => handle_self_test(command),
        Command::DictateRun {
            config,
            json_events,
            foreground,
        } => runtime::dictate_run::handle_dictate_run(runtime::dictate_run::DictateRunArgs {
            config,
            json_events,
            foreground,
            env_overrides: Vec::new(),
        }),
    }
}

/// Dispatch the `self-test` subcommand family. Every verb here is a pure,
/// headless regression check — no OS hooks, no audio, no display. Exits
/// non-zero on any failure so CI (and `wayland-user-smoke.sh`) can pin the
/// check without shelling out for platform detects.
fn handle_self_test(cmd: SelfTestCommand) -> anyhow::Result<()> {
    use whisper_dictate_app::hotkey::self_test::{
        features_available as ptt_features_available, run_ptt_wedge_test, SelfTestDriver,
    };
    use whisper_dictate_app::injection::self_test::{
        features_available as inj_features_available, run_injection_idempotency_test,
    };

    match cmd {
        SelfTestCommand::PttWedge {
            iterations,
            json,
            driver,
        } => {
            if iterations == 0 {
                return Err(anyhow::anyhow!(
                    "--iterations must be at least 1 (0 would be a vacuous pass)"
                ));
            }
            // Reject typo'd `--driver` BEFORE running the test, matching the
            // `hotkey capture --driver` policy (a smoke-script mis-spelling
            // should fail fast, not silently pick the auto backend).
            let parsed_driver = SelfTestDriver::parse(&driver).ok_or_else(|| {
                anyhow::anyhow!(
                    "--driver expects auto | rdev | evdev (or the x11 / wayland aliases); \
                     got {driver:?}"
                )
            })?;
            // Stock builds cannot exercise the guard bracket semantics (the
            // injector's `arm_start` lives behind `rust-injection`) — a "pass"
            // there would be a false negative and mask a real regression.
            // Surface an actionable rebuild message and exit non-zero.
            if !ptt_features_available() {
                return Err(anyhow::anyhow!(
                    "self-test ptt-wedge requires the `rust-hotkeys` and `rust-injection` \
                     cargo features - rebuild with \
                     `cargo build --features rust-hotkeys,rust-injection`"
                ));
            }
            let report = run_ptt_wedge_test(iterations, parsed_driver);
            if json {
                println!("{}", report.to_json());
            } else {
                print!("{}", report.to_plain());
            }
            if report.all_passed() {
                Ok(())
            } else {
                // Non-zero exit so CI trips. The report already printed the
                // per-iteration detail; a bare error keeps the tail short.
                Err(anyhow::anyhow!(
                    "self-test ptt-wedge failed (see report above for the failing iteration and stage)"
                ))
            }
        }
        SelfTestCommand::InjectionIdempotency {
            iterations,
            json,
            backend,
            live,
        } => {
            if iterations == 0 {
                return Err(anyhow::anyhow!(
                    "--iterations must be at least 1 (0 would be a vacuous pass)"
                ));
            }
            // Same feature-gate policy as ptt-wedge: on a stock build the
            // idempotency assertions can't fire (both the plan builder and
            // the guard bracket counter live behind those features), so a
            // "pass" would be a false negative. Surface a rebuild message.
            if !inj_features_available() {
                return Err(anyhow::anyhow!(
                    "self-test injection-idempotency requires the `rust-hotkeys` and \
                     `rust-injection` cargo features - rebuild with \
                     `cargo build --features rust-hotkeys,rust-injection`"
                ));
            }
            if live {
                // Loud stderr warning BEFORE any execution — mirrors the
                // `inject-text --do-it` policy so an operator who typed
                // `--live` by mistake sees the warning while they still
                // have a chance to Ctrl-C.
                eprintln!(
                    "warning: `self-test injection-idempotency --live` is REAL and will \
                     type into the active window on every iteration. Focus a scratch \
                     window NOW or Ctrl-C to abort."
                );
                // Codex #518 F5: on `--live --backend paste` the OS
                // clipboard is a shared resource. The harness inserts a
                // small spacer between iterations to let async clipboard
                // writes flush, but the operator is responsible for
                // ensuring the scratch window doesn't retain stale
                // content across iterations — surface this as a
                // documented limitation so nobody mistakes the run for a
                // hermetic test.
                if backend == "paste" {
                    eprintln!(
                        "warning: `--live --backend paste` shares the OS clipboard between \
                         iterations. Stale clipboard content from prior sessions may leak; \
                         inspect each iteration's pasted output manually rather than trusting \
                         the summary alone."
                    );
                }
            }
            let report = run_injection_idempotency_test(iterations, &backend, live);
            if json {
                println!("{}", report.to_json());
            } else {
                print!("{}", report.to_plain());
            }
            if report.all_passed() {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "self-test injection-idempotency failed (see report above for the failing iteration and stage)"
                ))
            }
        }
        SelfTestCommand::AudioCapture {
            duration_ms,
            device,
            json,
            fail_on_silence,
        } => handle_audio_capture_self_test(duration_ms, device, json, fail_on_silence),
        SelfTestCommand::WhisperLoad { model, json } => handle_whisper_load(&model, json),
        SelfTestCommand::Feedback { delay_ms, json } => handle_self_test_feedback(delay_ms, json),
        SelfTestCommand::AudioDucking { duration_ms, json } => {
            handle_self_test_audio_ducking(duration_ms, json)
        }
        SelfTestCommand::ProfileMatch {
            title,
            process,
            json,
        } => handle_self_test_profile_match(&title, &process, json),
        SelfTestCommand::HistoryWrite { text, json } => handle_self_test_history_write(&text, json),
        SelfTestCommand::MetricsWrite { text, json } => handle_self_test_metrics_write(&text, json),
        SelfTestCommand::Preview {
            frames,
            frame_samples,
            sample_rate,
            interval_ms,
            canned_text,
            json,
        } => handle_self_test_preview(
            frames,
            frame_samples,
            sample_rate,
            interval_ms,
            canned_text,
            json,
        ),
        SelfTestCommand::HotkeyBoot {
            hold_ms,
            chord,
            json,
            driver,
        } => handle_self_test_hotkey_boot(hold_ms, &chord, json, &driver),
    }
}

/// Dispatch `self-test hotkey-boot`. Exercises the SAME
/// [`whisper_dictate_app::hotkey::install_hotkey`] path the Phase-B
/// supervisor uses so a Windows-side wedge is reproducible from
/// PowerShell with visible stderr (the GUI binary runs under the
/// Windows GUI subsystem attribute and discards its stderr).
///
/// The `--driver` flag lets the operator pin a specific backend —
/// most importantly `register` on Windows, which is the RegisterHotKey
/// driver the GUI binary uses to bypass the WH_KEYBOARD_LL hook chain.
/// Reproducing a GUI-side wedge from PowerShell needs the same
/// driver, so this flag mirrors `hotkey capture --driver`.
fn handle_self_test_hotkey_boot(
    hold_ms: u64,
    chord: &str,
    json: bool,
    driver: &str,
) -> anyhow::Result<()> {
    use whisper_dictate_app::hotkey::boot_self_test::{
        features_available, reconcile_config_load, resolve_chord, run_boot_test,
    };
    if !features_available() {
        return Err(anyhow::anyhow!(
            "self-test hotkey-boot requires the `rust-hotkeys` and `rust-injection` \
             cargo features - rebuild with \
             `cargo build --features rust-hotkeys,rust-injection`"
        ));
    }
    // Route the driver preference through the same env var the
    // shipping install path consults. Reject unrecognised values BEFORE
    // installing anything so a typo does not silently fall back to
    // Auto — matches the `hotkey capture --driver` policy.
    whisper_dictate_app::hotkey::capture::validate_driver_flag(driver)?;
    std::env::set_var("VOICEPI_HOTKEY_DRIVER", driver);
    // Fetch the on-disk config's `key` field so a bare invocation
    // uses the same chord the supervisor would. Codex P2 #644 finding
    // r3658983556: a bare `unwrap_or_default()` masked a corrupt-config
    // I/O / parse failure and re-emerged as the misleading "no PTT
    // chord configured" message below, hiding the actual root cause
    // an operator debugging a wedge needs. The branching lives in the
    // pure helper `reconcile_config_load` so the "propagate the load
    // error when there is no override, otherwise warn-and-continue"
    // behaviour is directly unit-testable.
    let load_result = whisper_dictate_app::config::load_settings()
        .map(|s| s.key)
        .map_err(|err| err.to_string());
    let had_load_err = load_result.is_err();
    let config_key =
        reconcile_config_load(chord, load_result).map_err(|msg| anyhow::anyhow!(msg))?;
    if had_load_err {
        // Codex P2 #668 discussion 3665200198 (main.rs:324): a plain
        // `eprintln!` panics on `write_all` failure, and a self-test
        // invoked from a hidden Windows launcher or with a closed /
        // redirected stderr consumer would abort the CLI before
        // `run_boot_test` ever runs — the exact class of failure the
        // same commit fixed inside `diag::write_line`. Route through
        // the fallible-writer path (`diag::write_line`) instead so a
        // dead stderr is swallowed and the boot self-test still emits
        // its final report to stdout.
        whisper_dictate_app::diag::write_line(
            "[self-test hotkey-boot] warning: config load failed; \
             continuing with --chord override",
        );
    }
    let resolved = resolve_chord(chord, &config_key);
    if resolved.is_empty() {
        return Err(anyhow::anyhow!(
            "no PTT chord configured: pass `--chord <chord>` or set one via \
             `whisper-dictate config set key ctrl_l+shift_l` first"
        ));
    }
    let report = run_boot_test(resolved, hold_ms);
    if json {
        println!("{}", report.to_json());
    } else {
        println!("{}", report.to_plain());
    }
    if report.ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test hotkey-boot failed (see report above for the driver / chord / error)"
        ))
    }
}

/// Dispatch `self-test feedback`.
fn handle_self_test_feedback(delay_ms: u64, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::feedback::{
        run_feedback_self_test, FeedbackOptions,
    };
    let report = run_feedback_self_test(FeedbackOptions {
        delay: std::time::Duration::from_millis(delay_ms),
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test feedback failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Dispatch `self-test audio-ducking`.
fn handle_self_test_audio_ducking(duration_ms: u64, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::audio_ducking::{
        run_audio_ducking_self_test, AudioDuckingOptions,
    };
    let report = run_audio_ducking_self_test(AudioDuckingOptions {
        duration: std::time::Duration::from_millis(duration_ms),
        force_enabled: None,
        force_level: None,
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test audio-ducking failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Dispatch `self-test profile-match`.
fn handle_self_test_profile_match(title: &str, process: &str, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::profile_match::{
        run_profile_match_self_test, ProfileMatchOptions,
    };
    let report = run_profile_match_self_test(ProfileMatchOptions {
        title: title.to_owned(),
        process: process.to_owned(),
        profiles_json_override: String::new(),
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test profile-match failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Dispatch `self-test history-write`.
fn handle_self_test_history_write(text: &str, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::history_write::{
        run_history_write_self_test, HistoryWriteOptions,
    };
    let report = run_history_write_self_test(HistoryWriteOptions {
        text: text.to_owned(),
        path_override: None,
        force_enabled: None,
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test history-write failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Dispatch `self-test metrics-write`.
fn handle_self_test_metrics_write(text: &str, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::metrics_write::{
        run_metrics_write_self_test, MetricsWriteOptions,
    };
    let report = run_metrics_write_self_test(MetricsWriteOptions {
        text: text.to_owned(),
        path_override: None,
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test metrics-write failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Dispatch `self-test preview`.
fn handle_self_test_preview(
    frames: usize,
    frame_samples: usize,
    sample_rate: u32,
    interval_ms: u64,
    canned_text: String,
    json: bool,
) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::preview::{run_preview_self_test, PreviewOptions};
    let report = run_preview_self_test(PreviewOptions {
        frames,
        frame_samples,
        sample_rate,
        interval: std::time::Duration::from_millis(interval_ms),
        canned_text,
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test preview failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Feature-on path for `self-test audio-capture` — opens the cpal input
/// stream via [`whisper_dictate_app::audio::self_test::run_audio_capture_test`]
/// and prints either a JSON envelope or a plain summary. Returns Err (and
/// exits non-zero) when the report says the capture failed so CI trips.
#[cfg(feature = "audio-capture")]
fn handle_audio_capture_self_test(
    duration_ms: u64,
    device: String,
    json: bool,
    fail_on_silence: bool,
) -> anyhow::Result<()> {
    use whisper_dictate_app::audio::self_test::{run_audio_capture_test, AudioCaptureOptions};
    // Reject nonsense before we open a device. `--duration-ms 0` is a
    // vacuous "pass"; refuse loudly.
    if duration_ms == 0 {
        return Err(anyhow::anyhow!(
            "--duration-ms must be at least 1 (0 would be a vacuous pass)"
        ));
    }
    // Warn under 100 ms — cpal callback intervals on WASAPI can approach
    // 20-40 ms, so sub-100 ms runs risk zero callbacks and a false FAIL
    // for reasons other than a real regression. Not a hard cap; just a
    // one-line hint on stderr the caller can pipe away.
    if duration_ms < 100 {
        eprintln!(
            "warning: --duration-ms {duration_ms} is below the recommended 100ms floor \
             - cpal may not deliver even one callback in that window"
        );
    }
    let opts = AudioCaptureOptions {
        duration: std::time::Duration::from_millis(duration_ms),
        device,
        fail_on_silence,
    };
    let report = run_audio_capture_test(opts);
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.is_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test audio-capture failed (see report above for the specific error)"
        ))
    }
}

/// Feature-on path for `dictate-mic` — captures live mic audio through the
/// Rust VAD-free pipeline and drives the in-process `DictateSession`.
#[cfg(feature = "audio-capture")]
fn handle_dictate_mic(device: &str, seconds: f64, json: bool) -> anyhow::Result<()> {
    whisper_dictate_app::dictate::mic::handle_dictate_mic(device, seconds, json)
}

/// Stock-build stub: `dictate-mic` needs the cpal capture pipeline, which is
/// only compiled under `audio-capture`. Emit the same actionable rebuild
/// message shape as the sibling audio verbs and exit non-zero.
#[cfg(not(feature = "audio-capture"))]
fn handle_dictate_mic(_device: &str, _seconds: f64, _json: bool) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "dictate-mic requires the `audio-capture` cargo feature; \
         rebuild with `cargo build --features audio-capture`"
    ))
}

/// Stock-build stub: the audio module isn't compiled in without the
/// `audio-capture` feature, so we can't open a cpal stream. Emit an
/// actionable rebuild message (matching the pattern the `ptt-wedge` and
/// `injection-idempotency` verbs use for their own feature gates) and
/// exit non-zero so CI / the smoke script pin-check trips.
#[cfg(not(feature = "audio-capture"))]
fn handle_audio_capture_self_test(
    _duration_ms: u64,
    _device: String,
    _json: bool,
    _fail_on_silence: bool,
) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "self-test audio-capture requires the `audio-capture` cargo feature - \
         rebuild with `cargo build --features audio-capture`"
    ))
}

/// Dispatch `self-test whisper-load`. Feature-gated: on a stock build we
/// return the same shape of "rebuild" error the sibling verbs return, so
/// the smoke script's `grep` on the message keeps working across verbs.
#[cfg(feature = "whisper-rs-local")]
fn handle_whisper_load(model: &str, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::whisper::self_test::run_whisper_load_test;
    let report = run_whisper_load_test(model)?;
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.ok {
        Ok(())
    } else {
        // Non-zero exit so CI trips. The report already printed the
        // details; the tail keeps the error concise.
        Err(anyhow::anyhow!(
            "self-test whisper-load failed: {} ({})",
            report.error.unwrap_or_default(),
            report.error_kind.unwrap_or("unknown"),
        ))
    }
}

#[cfg(not(feature = "whisper-rs-local"))]
fn handle_whisper_load(_model: &str, _json: bool) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "self-test whisper-load requires the `whisper-rs-local` cargo feature - \
         rebuild with `cargo build --features whisper-rs-local` (needs cmake + a \
         C/C++ toolchain on the build host)"
    ))
}

/// Dispatch the hidden `transcribe-wav` sub-command.
///
/// Real implementation lives in `whisper::dispatch` and is only compiled in
/// behind the `whisper-rs-local` feature (which pulls in whisper.cpp + CMake).
/// In a stock build the binary still exposes the sub-command - keeping the
/// CLI surface stable across feature builds - but exits non-zero with a
/// clear "feature not compiled in" message so the Python caller knows to
/// fall back to its in-process path.
///
/// `--probe` short-circuits before reading stdin or the model env var: it
/// exits 0 on a feature-enabled build and non-zero on a stock build, so the
/// Python wiring can cheaply check whether shelling out to this binary will
/// actually do whisper inference before committing to it for a dictation.
/// Note: ASCII-only strings here so the stderr message renders cleanly under
/// PowerShell / cmd.exe / hidden launchers and Rust UI subprocess logs
/// (AGENTS.md Windows-output rule).
#[cfg(feature = "whisper-rs-local")]
fn handle_transcribe_wav(probe: bool) -> anyhow::Result<()> {
    if probe {
        // Feature compiled in - probe succeeds without doing any work.
        return Ok(());
    }
    whisper_dictate_app::whisper::handle_transcribe_wav()
}

#[cfg(not(feature = "whisper-rs-local"))]
fn handle_transcribe_wav(_probe: bool) -> anyhow::Result<()> {
    // Same error for probe and real call: the Python caller treats any
    // non-zero exit as "Rust backend unavailable, fall back to in-process".
    Err(anyhow::anyhow!(
        "this build of whisper-dictate was compiled without the \
         `whisper-rs-local` feature; the Rust transcription backend is \
         unavailable - unset VOICEPI_TRANSCRIBE_BACKEND or install a build \
         with the feature enabled"
    ))
}

/// Wave 8-A: long-running in-process Whisper worker. See
/// [`whisper::dispatch::handle_transcribe_server`] for the wire protocol
/// and the per-request error contract; the stock-build fallback mirrors
/// `handle_transcribe_wav` above so the Python wrapper sees the same
/// "backend unavailable" exit code for either subcommand.
#[cfg(feature = "whisper-rs-local")]
fn handle_transcribe_server() -> anyhow::Result<()> {
    whisper_dictate_app::whisper::handle_transcribe_server()
}

#[cfg(not(feature = "whisper-rs-local"))]
fn handle_transcribe_server() -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "this build of whisper-dictate was compiled without the \
         `whisper-rs-local` feature; the long-running transcribe-server \
         is unavailable - install a build with the feature enabled"
    ))
}

/// Route the `inject-text` subcommand to either the legacy hidden helper
/// (`--mode {type|paste}` — Python worker path) or the public dry-run/inject
/// verb (`inject-text <TEXT> [--dry-run|--do-it] [--backend NAME] [--json]`).
///
/// Selection rules (kept simple so the shape is unit-testable):
///
/// * `mode` non-empty → legacy path via [`injection::handle_inject_text`].
///   Preserves the Python worker's on-disk contract without a shim.
/// * `text_arg` some → public path via
///   [`injection::handle_public_inject_text`].
/// * neither → error: the user didn't tell us what to inject. Prints a hint
///   at both invocation shapes so they know both exist.
fn dispatch_inject_text(cmd: Command) -> anyhow::Result<()> {
    // Destructuring the enum variant here keeps clippy's too-many-arguments
    // check happy while still giving us named locals for each field.
    let Command::InjectText {
        text_arg,
        dry_run,
        do_it,
        backend,
        json,
        mode,
        text,
        xkb_layout,
        target_title,
        target_process,
    } = cmd
    else {
        unreachable!("dispatch_inject_text called with non-InjectText variant")
    };
    if !mode.is_empty() {
        // Legacy hidden-helper path: honour --mode + --text + --xkb-layout,
        // exactly as before this PR. The public flags are ignored on this
        // path (they never coexist in the shipping Python invocation).
        return injection::handle_inject_text(
            &mode,
            &text,
            &xkb_layout,
            &target_title,
            &target_process,
        );
    }
    let Some(text_positional) = text_arg else {
        return Err(anyhow::anyhow!(
            "inject-text: pass TEXT as a positional argument \
             (e.g. `whisper-dictate inject-text \"smoke test\"`) or use the \
             legacy `--mode {{type|paste}} --text ...` helper form"
        ));
    };
    injection::handle_public_inject_text(
        &text_positional,
        &backend,
        dry_run,
        do_it,
        json,
        &target_title,
        &target_process,
    )
}

#[cfg(feature = "audio-capture")]
fn handle_devices_command() -> anyhow::Result<()> {
    whisper_dictate_app::devices::handle_devices()
}

#[cfg(not(feature = "audio-capture"))]
fn handle_devices_command() -> anyhow::Result<()> {
    // Stable, machine-readable refusal so the Python shell-out can detect
    // "not built with cpal" and fall back to its own enumeration without
    // parsing a free-form error message. Exits non-zero so subprocess.run's
    // returncode check trips the fallback path in vp_devices.
    println!(
        "{{\"error\":\"devices_unavailable\",\"reason\":\"binary built without audio-capture feature\"}}"
    );
    std::process::exit(2);
}

/// Handle `whisper-dictate devices test <NAME>`.
///
/// On `audio-capture` builds (the shipping binary) this dispatches to the
/// native cpal probe in [`audio::device_probe`] and prints the single-line
/// JSON envelope the UI parser in `ui::device_test` expects. Step 2 of the
/// `vp_device_test.py` retirement (issue #348) removed the Python fallback
/// altogether — the retired Python module and its `--test-audio-device`
/// argparse flag are gone.
///
/// On stock builds (no `audio-capture`) the subcommand is unavailable: the
/// binary emits a clear "rebuild with --features audio-capture" refusal on
/// stderr and exits non-zero. The shipping binary always ships with
/// `audio-capture`; only dev builds hit this path.
#[cfg(feature = "audio-capture")]
fn handle_devices_test(name: &str) -> anyhow::Result<()> {
    let result = whisper_dictate_app::audio::device_probe::probe_device(name);
    println!("{}", result.to_json_line());
    Ok(())
}

#[cfg(not(feature = "audio-capture"))]
fn handle_devices_test(_name: &str) -> anyhow::Result<()> {
    // Non-audio-capture dev-build regression documented in the step-2 PR:
    // there is no Python fallback to shell out to. Emit a machine-readable
    // refusal on stderr and exit non-zero so the caller can distinguish
    // "not built with the native probe" from an actual probe failure.
    eprintln!(
        "devices test is unavailable: this binary was built without the \
         `audio-capture` feature. Rebuild with `cargo build --features audio-capture`."
    );
    std::process::exit(2);
}
