#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;

use whisper_dictate_app::cli::{Cli, Command};
use whisper_dictate_app::{
    cloud_api, command_hook, config, dictionary, formatting, injection, model_capacity, privacy,
    profiles, redaction, runtime, telemetry, ui,
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
        Command::WorkerEvent => telemetry::handle_worker_event(),
        Command::CommandHook => command_hook::handle_command_hook(),
        Command::RedactText => redaction::handle_redact_text(),
        Command::ApplyProfile => profiles::handle_apply_profile(),
        Command::Privacy => privacy::handle_privacy(),
    }
}
