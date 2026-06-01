use anyhow::Result;
use clap::Parser;

use whisper_dictate_app::cli::{Cli, Command};
use whisper_dictate_app::{config, runtime, ui};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Command::Ui) {
        Command::Ui | Command::Settings => ui::run(),
        Command::Run => runtime::run_terminal(),
        Command::Doctor => runtime::doctor(),
        Command::Install => runtime::install(),
        Command::Config { command } => config::handle_command(command),
    }
}
