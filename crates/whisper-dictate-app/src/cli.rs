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
    /// Internal helper used by the Python worker for keyboard injection.
    #[command(hide = true)]
    InjectText {
        /// Injection mode to execute.
        #[arg(long, value_parser = ["type", "paste"])]
        mode: String,
        /// Text to inject for type mode.
        #[arg(long, default_value = "")]
        text: String,
        /// XKB layout used for Wayland direct keycode typing.
        #[arg(long, default_value = "")]
        xkb_layout: String,
        /// Captured target window title, when available.
        #[arg(long, default_value = "")]
        target_title: String,
        /// Captured target process name, when available.
        #[arg(long, default_value = "")]
        target_process: String,
    },
    /// Internal helper used by the Python worker for post-STT formatting.
    #[command(hide = true)]
    FormatText {
        /// Text to format.
        #[arg(long)]
        text: String,
        /// Spoken formatting command set: off, en, da, or both.
        #[arg(long, default_value = "off")]
        command_set: String,
    },
    /// Internal helper used by the Python worker for cloud STT.
    #[command(hide = true)]
    CloudTranscribe {
        /// OpenAI-compatible API base URL.
        #[arg(long)]
        base_url: String,
        /// API key.
        #[arg(long)]
        api_key: String,
        /// Transcription model.
        #[arg(long)]
        model: String,
        /// WAV audio file path.
        #[arg(long)]
        audio_wav_path: String,
        /// Optional spoken language hint.
        #[arg(long, default_value = "")]
        language: String,
        /// Optional transcription prompt.
        #[arg(long, default_value = "")]
        prompt: String,
        /// Request timeout in milliseconds.
        #[arg(long, default_value_t = 30000)]
        timeout_ms: u64,
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

    #[test]
    fn parses_hidden_inject_text_subcommand() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "inject-text",
            "--mode",
            "type",
            "--text",
            "høre",
            "--xkb-layout",
            "dk",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::InjectText {
                mode: "type".to_owned(),
                text: "høre".to_owned(),
                xkb_layout: "dk".to_owned(),
                target_title: String::new(),
                target_process: String::new(),
            })
        );
    }

    #[test]
    fn parses_hidden_format_text_subcommand() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "format-text",
            "--text",
            "første komma",
            "--command-set",
            "da",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::FormatText {
                text: "første komma".to_owned(),
                command_set: "da".to_owned(),
            })
        );
    }

    #[test]
    fn parses_hidden_cloud_transcribe_subcommand() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "cloud-transcribe",
            "--base-url",
            "https://api.openai.com/v1",
            "--api-key",
            "key",
            "--model",
            "gpt-4o-mini-transcribe",
            "--audio-wav-path",
            "audio.wav",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::CloudTranscribe {
                base_url: "https://api.openai.com/v1".to_owned(),
                api_key: "key".to_owned(),
                model: "gpt-4o-mini-transcribe".to_owned(),
                audio_wav_path: "audio.wav".to_owned(),
                language: String::new(),
                prompt: String::new(),
                timeout_ms: 30000,
            })
        );
    }
}
