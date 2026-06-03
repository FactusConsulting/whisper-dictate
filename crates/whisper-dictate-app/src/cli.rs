use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "whisper-dictate")]
#[command(bin_name = "whisper-dictate")]
#[command(about = "Desktop and terminal controller for whisper-dictate")]
pub struct Cli {
    /// Print version and exit.
    #[arg(long)]
    pub version: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Open the desktop UI.
    Ui,
    /// Open settings in the desktop UI.
    Settings,
    /// Run dictation in the terminal.
    Run {
        /// Arguments passed through to voice_pi.py.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Check runtime dependencies and platform readiness.
    Doctor,
    /// Install or repair local runtime dependencies.
    Install,
    /// Run the Ubuntu Wayland desktop setup helper.
    SetupUbuntu,
    /// Show local GPU VRAM and model-fit guidance.
    ModelCapacity,
    /// Inspect configuration paths and values.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Manage the custom dictionary without starting Python.
    Dictionary {
        #[command(subcommand)]
        command: DictionaryCommand,
    },
    /// Inspect local dictation history without starting Python.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ConfigCommand {
    /// Print the config file path used by the controller.
    Path,
    /// Print the raw JSON config, or an empty object if no config exists.
    Show,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum DictionaryCommand {
    /// Print dictionary path, term count, replacement count and prompt preview.
    Status,
    /// Create the dictionary if needed and open it in the platform editor.
    Open,
    /// Add a prompt vocabulary term.
    Add {
        /// Term to add.
        term: String,
    },
    /// Add or update a deterministic replacement in FROM=TO form.
    Replace {
        /// Replacement mapping, for example "lead death=lead dev".
        mapping: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum HistoryCommand {
    /// List recent history rows.
    List {
        /// Number of rows to show.
        #[arg(default_value_t = 10)]
        limit: usize,
    },
    /// Print the most recent history text.
    Last,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_subcommand_opens_ui_by_default() {
        let cli = Cli::parse_from(["whisper-dictate"]);
        assert!(!cli.version);
        assert_eq!(cli.command, None);
    }

    #[test]
    fn parses_version_flag() {
        let cli = Cli::parse_from(["whisper-dictate", "--version"]);
        assert!(cli.version);
        assert_eq!(cli.command, None);
    }

    #[test]
    fn parses_run_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "run"]);
        assert_eq!(cli.command, Some(Command::Run { args: vec![] }));
    }

    #[test]
    fn parses_run_passthrough_args() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "run",
            "--key",
            "shift_r+ctrl_r",
            "--lang",
            "da",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::Run {
                args: vec![
                    "--key".to_owned(),
                    "shift_r+ctrl_r".to_owned(),
                    "--lang".to_owned(),
                    "da".to_owned(),
                ],
            })
        );
    }

    #[test]
    fn parses_settings_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "settings"]);
        assert_eq!(cli.command, Some(Command::Settings));
    }

    #[test]
    fn parses_setup_ubuntu_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "setup-ubuntu"]);
        assert_eq!(cli.command, Some(Command::SetupUbuntu));
    }

    #[test]
    fn parses_model_capacity_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "model-capacity"]);
        assert_eq!(cli.command, Some(Command::ModelCapacity));
    }

    #[test]
    fn parses_config_path_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "config", "path"]);
        assert_eq!(
            cli.command,
            Some(Command::Config {
                command: ConfigCommand::Path,
            })
        );
    }

    #[test]
    fn parses_dictionary_add_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "dictionary", "add", "Codex"]);
        assert_eq!(
            cli.command,
            Some(Command::Dictionary {
                command: DictionaryCommand::Add {
                    term: "Codex".to_owned(),
                },
            })
        );
    }

    #[test]
    fn parses_history_list_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "history", "list", "25"]);
        assert_eq!(
            cli.command,
            Some(Command::History {
                command: HistoryCommand::List { limit: 25 },
            })
        );
    }
}
