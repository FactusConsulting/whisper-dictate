use clap::Subcommand;

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ConfigCommand {
    /// Print the config file path used by the controller.
    Path,
    /// Print the raw JSON config, or an empty object if no config exists.
    Show,
    /// Print the current value of a single setting key.
    ///
    /// The key must be one of the settings owned by the typed `AppSettings`
    /// (`stt_backend`, `audio_device`, `model`, ...). Unknown keys exit 1 with
    /// an error message that lists every valid key. Emits the stored string
    /// form (boolean settings are stored as `"1"` / `"0"`) unless `--json` is
    /// passed, in which case the output is a single-line
    /// `{"key": "...", "value": ...}` envelope.
    Get {
        /// Setting key (e.g. `audio_device`, `model`, `stt_backend`).
        key: String,
        /// Emit machine-readable JSON: `{"key": "...", "value": ...}`.
        #[arg(long)]
        json: bool,
        /// Override the config file path (default: platform user config).
        /// Also honours `VOICEPI_CONFIG` when this flag is omitted.
        #[arg(long, value_name = "PATH")]
        config: Option<String>,
    },
    /// Set a single setting key, validate, and persist the new config file.
    ///
    /// The value is validated via the same `AppSettings::validate` path the
    /// UI uses on save - invalid enum values or unparseable numbers fail
    /// cleanly WITHOUT touching the file on disk. Booleans accept
    /// `1`/`0`/`true`/`false`/`yes`/`no`/`on`/`off` (case-insensitive) and
    /// are normalised to the `"1"`/`"0"` form the worker expects.
    Set {
        /// Setting key (e.g. `audio_device`, `model`, `stt_backend`).
        key: String,
        /// New value for `key`. Empty string clears the setting (equivalent
        /// to removing the key from the config file - the worker will fall
        /// back to the schema default). `allow_hyphen_values` lets a value
        /// that happens to begin with `-` (e.g. an audio device name or a
        /// negative dBFS) through clap.
        #[arg(allow_hyphen_values = true)]
        value: String,
        /// Override the config file path (default: platform user config).
        /// Also honours `VOICEPI_CONFIG` when this flag is omitted.
        #[arg(long, value_name = "PATH")]
        config: Option<String>,
    },
    /// List every settings key with its current value, sorted alphabetically.
    /// Handy for shell completion and for scripting a "show me everything"
    /// dump without pretty-printing the entire config file.
    List {
        /// Emit machine-readable JSON: an object of `{key: value, ...}`.
        #[arg(long)]
        json: bool,
        /// Override the config file path (default: platform user config).
        #[arg(long, value_name = "PATH")]
        config: Option<String>,
    },
}
