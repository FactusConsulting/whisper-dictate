#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;

use whisper_dictate_app::cli::{Cli, Command};
use whisper_dictate_app::{
    cloud_api, config, dictionary, formatting, injection, model_capacity, runtime, telemetry, ui,
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
        Command::ModelCapacity => model_capacity::handle_command(),
        Command::Config { command } => config::handle_command(command),
        Command::Dictionary { command } => dictionary::handle_command(command),
        Command::History { command } => telemetry::handle_history_command(command),
        Command::InjectText {
            mode,
            text,
            xkb_layout,
            target_title,
            target_process,
        } => injection::handle_inject_text(
            &mode,
            &text,
            &xkb_layout,
            &target_title,
            &target_process,
        ),
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
    }
}
