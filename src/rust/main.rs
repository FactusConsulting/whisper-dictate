#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;

use whisper_dictate_app::cli::{Cli, Command, DevicesCommand};
use whisper_dictate_app::{
    benchmark, cloud_api, command_hook, config, corpus_record, dictate, dictionary, formatting,
    health, history, hotkey, injection, model_capacity, postprocess, privacy, profiles, redaction,
    runtime, telemetry, ui, whisper,
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
        Command::Doctor => runtime::doctor(),
        Command::Bench => benchmark::handle_bench(),
        Command::CorpusRecord { id } => corpus_record::handle_corpus_record(&id),
        Command::SimulatePtt {
            wav,
            inject,
            language,
            model,
            json,
        } => runtime::run_terminal(simulate_ptt_args(&wav, inject, &language, &model, json)),
        Command::Install => runtime::install(),
        Command::SetupUbuntu => runtime::setup_ubuntu(),
        Command::ModelCapacity { json } => model_capacity::handle_command(json),
        Command::Config { command } => config::handle_command(command),
        Command::Dictionary { command } => dictionary::handle_command(command),
        Command::DictionaryRuntime => dictionary::handle_runtime(),
        Command::DictionaryOps => dictionary::handle_ops(),
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
    }
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

#[cfg(feature = "audio-in-rust")]
fn handle_devices_command() -> anyhow::Result<()> {
    whisper_dictate_app::devices::handle_devices()
}

#[cfg(not(feature = "audio-in-rust"))]
fn handle_devices_command() -> anyhow::Result<()> {
    // Stable, machine-readable refusal so the Python shell-out can detect
    // "not built with cpal" and fall back to its own enumeration without
    // parsing a free-form error message. Exits non-zero so subprocess.run's
    // returncode check trips the fallback path in vp_devices.
    println!("{{\"error\":\"devices_unavailable\",\"reason\":\"binary built without audio-in-rust feature\"}}");
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
