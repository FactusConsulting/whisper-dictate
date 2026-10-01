//! Public CLI entry point; command-family definitions live in `cli/`.
use clap::Parser;

#[path = "cli/commands.rs"]
mod commands;
#[path = "cli/config.rs"]
mod config;
#[path = "cli/devices.rs"]
mod devices;
#[path = "cli/dictionary.rs"]
mod dictionary;
#[path = "cli/history.rs"]
mod history;
#[path = "cli/hotkey.rs"]
mod hotkey;
#[path = "cli/models.rs"]
mod models;
#[path = "cli/self_test.rs"]
mod self_test;

pub use commands::Command;
pub use config::ConfigCommand;
pub use devices::DevicesCommand;
pub use dictionary::DictionaryCommand;
pub use history::HistoryCommand;
pub use hotkey::HotkeyCommand;
pub use models::ModelsCommand;
pub use self_test::SelfTestCommand;

#[derive(Debug, Parser)]
#[command(name = "wd")]
#[command(bin_name = "wd")]
#[command(about = "Desktop and terminal controller for WD")]
pub struct Cli {
    /// Print version and exit.
    #[arg(long)]
    pub version: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
