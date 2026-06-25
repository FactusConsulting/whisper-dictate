#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;

use whisper_dictate_app::cli::{Cli, Command};
use whisper_dictate_app::{
    cloud_api, command_hook, config, dictionary, formatting, health, injection, model_capacity,
    privacy, profiles, redaction, runtime, telemetry, ui,
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
        Command::Install => runtime::install(),
        Command::SetupUbuntu => runtime::setup_ubuntu(),
        Command::ModelCapacity { json } => model_capacity::handle_command(json),
        Command::Config { command } => config::handle_command(command),
        Command::Dictionary { command } => dictionary::handle_command(command),
        Command::DictionaryRuntime => dictionary::handle_runtime(),
        Command::DictionaryOps => dictionary::handle_ops(),
        Command::History { command } => telemetry::handle_history_command(command),
        Command::InjectText {
            mode,
            text,
            xkb_layout,
            target_title,
            target_process,
        } => {
            injection::handle_inject_text(&mode, &text, &xkb_layout, &target_title, &target_process)
        }
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
        Command::Health => health::handle_health(),
        Command::TranscribeWav { probe } => handle_transcribe_wav(probe),
        Command::Inject => injection::handle_inject(),
        Command::Devices => handle_devices_command(),
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
