#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;

use whisper_dictate_app::cli::{Cli, Command, DevicesCommand, SelfTestCommand};
use whisper_dictate_app::{
    benchmark, cloud_api, command_hook, config, corpus_record, dictate, dictionary, doctor,
    formatting, health, history, hotkey, injection, model_capacity, postprocess, privacy, profiles,
    redaction, runtime, telemetry, ui, whisper,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
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
        Command::Doctor { json, config } => doctor::handle_doctor(json, config.as_deref()),
        Command::Bench => benchmark::handle_bench(),
        Command::CorpusRecord { id } => corpus_record::handle_corpus_record(&id),
        Command::SimulatePtt {
            wav,
            inject,
            language,
            model,
            json,
        } => runtime::run_terminal(simulate_ptt_args(&wav, inject, &language, &model, json)),
        Command::SimulateSession { wav, json, repeat } => {
            dictate::simulate::handle_simulate_session(&wav, json, repeat)
        }
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
            &api_key,
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
            Some(DevicesCommand::Test { name }) => runtime::run_terminal(devices_test_args(&name)),
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
                     cargo features — rebuild with \
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
                     `rust-injection` cargo features — rebuild with \
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
             — cpal may not deliver even one callback in that window"
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
        "self-test audio-capture requires the `audio-capture` cargo feature — \
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
        "self-test whisper-load requires the `whisper-rs-local` cargo feature — \
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

/// Build the Python argv for the `simulate-ptt` subcommand.
///
/// The Rust `simulate-ptt` command is a thin front for the Python worker's
/// `--simulate-ptt` flag: this helper translates the parsed clap values back
/// into the `--flag value` argv the Python argparse expects, skipping
/// empty-string optional values so `_resolve_device` / `MODEL_NAME` fall
/// back to their configured defaults. Kept as a pure function so the
/// argv-shape is unit-testable without spawning Python.
///
/// No `--inject-mode` is forwarded: only the direct-typing strategy is
/// implemented in this POC, so exposing a selector would let the caller
/// ask for a mode the pipeline can't actually deliver.
fn simulate_ptt_args(
    wav: &str,
    inject: bool,
    language: &str,
    model: &str,
    json: bool,
) -> Vec<String> {
    let mut args = vec![
        "--simulate-ptt".to_owned(),
        "--wav".to_owned(),
        wav.to_owned(),
    ];
    if inject {
        args.push("--inject".to_owned());
    }
    if !language.trim().is_empty() {
        args.push("--lang".to_owned());
        args.push(language.to_owned());
    }
    if !model.trim().is_empty() {
        args.push("--model".to_owned());
        args.push(model.to_owned());
    }
    if json {
        args.push("--json".to_owned());
    }
    args
}

/// Build the Python argv for `devices test <NAME>`.
///
/// The Rust `devices test` subcommand is a thin front for the Python worker's
/// `--test-audio-device` query mode (`vp_device_test.test_audio_device`),
/// which reuses the SAME WASAPI/DirectSound/MME open matrix as live capture
/// (see `vp_capture._start_sounddevice`). Loads no ML model — the query mode
/// short-circuits before the model-load path in `runtime.py`.
///
/// Kept as a pure function so the argv shape is unit-testable without
/// spawning Python. Empty `name` is preserved verbatim: the Python side treats
/// `""` as "test the system default input", which is a documented use case
/// (e.g. headless CI containers where no named device is available).
fn devices_test_args(name: &str) -> Vec<String> {
    vec!["--test-audio-device".to_owned(), name.to_owned()]
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
    println!("{{\"error\":\"devices_unavailable\",\"reason\":\"binary built without audio-capture feature\"}}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::{devices_test_args, simulate_ptt_args};

    #[test]
    fn devices_test_args_forwards_name_verbatim() {
        let args = devices_test_args("Microphone (Yeti Classic)");
        assert_eq!(
            args,
            vec![
                "--test-audio-device".to_owned(),
                "Microphone (Yeti Classic)".to_owned(),
            ]
        );
    }

    #[test]
    fn devices_test_args_preserves_empty_string_for_system_default() {
        // Empty string is the documented way to test the system default input;
        // it MUST survive the CLI hop unchanged so the Python query mode sees
        // "" and picks device=None.
        let args = devices_test_args("");
        assert_eq!(args, vec!["--test-audio-device".to_owned(), String::new()]);
    }

    #[test]
    fn devices_test_args_does_not_forward_extra_flags() {
        // Only --test-audio-device NAME is forwarded. No stray flags that could
        // accidentally load a model or open a hotkey listener — the Python
        // query mode short-circuits before those code paths.
        let args = devices_test_args("mic");
        assert!(!args.iter().any(|a| a == "--simulate-ptt"));
        assert!(!args.iter().any(|a| a == "--capture-hotkey"));
        assert!(!args.iter().any(|a| a == "--run-benchmark"));
    }

    #[test]
    fn simulate_ptt_args_dry_run_default() {
        let args = simulate_ptt_args("hello.wav", false, "", "", false);
        assert_eq!(
            args,
            vec![
                "--simulate-ptt".to_owned(),
                "--wav".to_owned(),
                "hello.wav".to_owned(),
            ]
        );
    }

    #[test]
    fn simulate_ptt_args_forwards_all_flags() {
        let args = simulate_ptt_args("path/hello.wav", true, "da", "tiny.en", true);
        assert_eq!(
            args,
            vec![
                "--simulate-ptt".to_owned(),
                "--wav".to_owned(),
                "path/hello.wav".to_owned(),
                "--inject".to_owned(),
                "--lang".to_owned(),
                "da".to_owned(),
                "--model".to_owned(),
                "tiny.en".to_owned(),
                "--json".to_owned(),
            ]
        );
    }

    #[test]
    fn simulate_ptt_args_skips_empty_language_and_model() {
        let args = simulate_ptt_args("hello.wav", false, "   ", "  ", false);
        // Blank language / model must NOT be forwarded — the Python side falls
        // back to the configured default (VOICEPI_MODEL / auto-detect language)
        // exactly like the live PTT loop does.
        assert!(!args.contains(&"--lang".to_owned()));
        assert!(!args.contains(&"--model".to_owned()));
    }

    #[test]
    fn simulate_ptt_args_never_forwards_inject_mode() {
        // POC-scope guard (fixes a Claude review finding from PR #491): only
        // the direct-typing strategy is wired up right now, so the argv-builder
        // MUST NOT emit a `--inject-mode` flag — the reported inject_strategy
        // would otherwise lie about paste vs type. When paste is implemented,
        // a follow-up can re-add the selector plus the wiring behind it.
        let args = simulate_ptt_args("hello.wav", true, "da", "tiny.en", true);
        assert!(!args.contains(&"--inject-mode".to_owned()));
    }
}
