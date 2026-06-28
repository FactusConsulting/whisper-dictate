use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "whisper-dictate")]
#[command(bin_name = "whisper-dictate")]
#[command(about = "Desktop and terminal controller for whisper-dictate")]
pub struct Cli {
    /// Print version and exit.
    #[arg(long)]
    pub version: bool,

    /// Issue #326: forward a "toggle recording" command to the running
    /// whisper-dictate daemon (start if idle, stop-and-transcribe if
    /// recording) and exit. On Linux this sends SIGUSR1 to the PID from
    /// the daemon's PID file; on macOS/Windows it currently errors with a
    /// clear "not implemented" message (Linux-only IPC for now, per #326).
    /// Mutually exclusive with `--start-recording`, `--stop-recording`,
    /// `--cancel-recording`, and `--version` (clap-enforced). When combined
    /// with a subcommand the flag wins — `main.rs` short-circuits to the
    /// IPC forwarder before the subcommand match runs, matching the documented
    /// "exactly one top-level action per invocation" contract.
    #[arg(
        long = "toggle-recording",
        conflicts_with_all = [
            "start_recording", "stop_recording", "cancel_recording", "version",
        ],
    )]
    pub toggle_recording: bool,

    /// Forward a "start recording" command to the running daemon. On the
    /// wire this still uses SIGUSR1 (Unix signals carry no data); the CLI
    /// writes the action token to a small command file so the daemon
    /// distinguishes start / stop / toggle. No-op if the daemon is already
    /// recording.
    #[arg(
        long = "start-recording",
        conflicts_with_all = [
            "toggle_recording", "stop_recording", "cancel_recording", "version",
        ],
    )]
    pub start_recording: bool,

    /// Forward a "stop and transcribe" command to the running daemon. Same
    /// wire as `--start-recording` (SIGUSR1 + command file). No-op if no
    /// recording is in flight.
    #[arg(
        long = "stop-recording",
        conflicts_with_all = [
            "toggle_recording", "start_recording", "cancel_recording", "version",
        ],
    )]
    pub stop_recording: bool,

    /// Forward a "cancel recording" command to the running daemon
    /// (discard buffered audio, no transcription). Uses SIGUSR2 so it
    /// works even when the command file is missing — the documented
    /// "emergency stop" hotkey.
    #[arg(
        long = "cancel-recording",
        conflicts_with_all = [
            "toggle_recording", "start_recording", "stop_recording", "version",
        ],
    )]
    pub cancel_recording: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// Map the four external-toggle flags onto an [`crate::runtime::external_toggle::ExternalCommand`].
    /// Returns `None` when none of the flags are set; callers in `main.rs`
    /// use this to short-circuit before dispatching the subcommand.
    /// Mutual exclusion is enforced by clap (`conflicts_with_all`); this
    /// helper just maps the matched flag to the IPC enum.
    pub fn external_command(&self) -> Option<crate::runtime::external_toggle::ExternalCommand> {
        use crate::runtime::external_toggle::ExternalCommand;
        if self.toggle_recording {
            Some(ExternalCommand::Toggle)
        } else if self.start_recording {
            Some(ExternalCommand::Start)
        } else if self.stop_recording {
            Some(ExternalCommand::Stop)
        } else if self.cancel_recording {
            Some(ExternalCommand::Cancel)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Open the desktop UI.
    Ui,
    /// Open settings in the desktop UI.
    Settings,
    /// Run dictation in the terminal.
    Run {
        /// Arguments passed through to the Python runtime module.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Check runtime dependencies and platform readiness.
    Doctor,
    /// Run the golden benchmark corpus through the configured backend and
    /// print a one-line `[benchmark] ...` summary. Same code path as the
    /// "Run benchmark" button — the corpus is resolved relative to the app
    /// root (or per-user appdata) and the configured backend is used.
    #[command(alias = "benchmark")]
    Bench,
    /// Record reference audio for a golden-corpus item from the configured
    /// microphone and save it to `<appdata>/benchmark/audio/<id>.wav`.
    /// `id` must match `[A-Za-z0-9._-]+` (the same filename-stem allowlist the
    /// UI picker enforces).
    CorpusRecord {
        /// Corpus item id (filename stem under `<appdata>/benchmark/audio/`).
        ///
        /// `allow_hyphen_values` is required so clap accepts manifest ids that
        /// happen to start with `-` (e.g. `-sample`). The full allowlist is
        /// then enforced by `corpus_record::is_safe_corpus_id` BEFORE shelling
        /// out — defence in depth on top of the worker's own guard.
        #[arg(allow_hyphen_values = true)]
        id: String,
    },
    /// Install or repair local runtime dependencies.
    Install,
    /// Run the Ubuntu Wayland desktop setup helper.
    SetupUbuntu,
    /// Show local GPU VRAM and model-fit guidance.
    ModelCapacity {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
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
    /// Internal helper used by the Python worker for dictionary prompt and replacements.
    #[command(hide = true)]
    DictionaryRuntime,
    /// Internal helper used by the Python worker for the dictionary training /
    /// suggestion ops (Wave 4-A shell-out fallback for
    /// `VOICEPI_DICTIONARY_BACKEND=rust`). Reads a JSON envelope on stdin
    /// (`{"op": "...", "params": {...}}`) and writes a JSON response on
    /// stdout. See `src/rust/dictionary/ops.rs` for the op catalogue.
    #[command(hide = true)]
    DictionaryOps,
    /// Internal helper used by the Python worker for the dictation
    /// orchestrator pure-logic decisions (Wave 5 shell-out fallback for
    /// `VOICEPI_DICTATE_BACKEND=rust`). Reads a JSON envelope on stdin
    /// (`{"op": "...", "params": {...}}`) and writes a JSON response on
    /// stdout. See `src/rust/dictate/ops.rs` for the op catalogue.
    #[command(hide = true)]
    DictateOps,
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
    /// Internal helper used by the Python worker to append JSONL safely.
    #[command(hide = true)]
    AppendJsonl {
        /// JSONL file to append to.
        #[arg(long)]
        path: String,
    },
    /// Internal helper used by the Python worker to append filtered history.
    #[command(hide = true)]
    AppendHistory {
        /// History JSONL file to append to.
        #[arg(long)]
        path: String,
    },
    /// Internal helper used by the Python worker to append metrics + history.
    #[command(hide = true)]
    AppendRecordSinks,
    /// Internal helper used by the Python worker to emit controller events.
    #[command(hide = true)]
    WorkerEvent,
    /// Internal helper used by the Python worker to run command hooks.
    #[command(hide = true)]
    CommandHook,
    /// Internal helper used by the Python worker for cloud-safe redaction.
    #[command(hide = true)]
    RedactText,
    /// Internal helper used by the Python worker for target profile matching.
    #[command(hide = true)]
    ApplyProfile,
    /// Internal helper used by the Python worker for local-only checks.
    #[command(hide = true)]
    Privacy,
    /// Internal helper used by the Python worker for post-STT formatting /
    /// LLM cleanup. JSON envelope on stdin, JSON response on stdout — see
    /// `src/rust/postprocess.rs`. Gated at runtime by
    /// `VOICEPI_POSTPROCESS_BACKEND=rust`; default install keeps the Python
    /// path.
    #[command(hide = true)]
    Postprocess,
    /// Internal helper used by the Python worker for OpenAI-compatible chat
    /// completion (post-processor cloud backend) + transcription prompt
    /// capping. JSON envelope on stdin, JSON response on stdout — see
    /// `src/rust/cloud_api/chat.rs`. Gated at runtime by
    /// `VOICEPI_EXTERNAL_API_BACKEND=rust`.
    #[command(hide = true)]
    ExternalApi,
    /// Internal helper: render the `[health]` line or compute the 4-level grade.
    #[command(hide = true)]
    Health,
    /// Internal helper used by the Python worker for local Whisper inference
    /// when `VOICEPI_TRANSCRIBE_BACKEND=rust`. Only does real work when the
    /// binary was built with `--features whisper-rs-local`; otherwise exits
    /// non-zero with a clear "feature not compiled in" message so the Python
    /// caller falls back to its own path. JSON request on stdin, JSON
    /// response on stdout — see `src/rust/whisper/dispatch.rs`.
    ///
    /// The `--probe` flag short-circuits before reading stdin / the model env
    /// var: it exits 0 on a feature-enabled build and non-zero on a stock
    /// build, so the Python caller can cheaply check whether the binary
    /// actually supports the Rust backend before committing to it for a
    /// dictation.
    #[command(hide = true)]
    TranscribeWav {
        /// Probe-only mode: do not read stdin or run inference; exit 0 iff
        /// the binary was built with `--features whisper-rs-local`. Used by
        /// the Python wiring to gate `RustWhisperShellModel` so an
        /// accidentally-enabled `VOICEPI_TRANSCRIBE_BACKEND=rust` against a
        /// stock build falls back to faster-whisper instead of failing the
        /// first dictation.
        #[arg(long)]
        probe: bool,
    },
    /// Long-running in-process Whisper worker. Reads one JSON request per
    /// `\n`-terminated line from stdin, writes one JSON response per line to
    /// stdout — same envelope as `transcribe-wav` but the GGML model is
    /// loaded ONCE and stays resident between calls (subject to
    /// `VOICEPI_WHISPER_IDLE_UNLOAD_S`). Per-request errors stay in-protocol
    /// as `{"error":"..."}` envelopes so a single bad request does not tear
    /// down the worker. See `src/rust/whisper/dispatch.rs::handle_transcribe_server`.
    ///
    /// First stdout line is a `ServerReady` envelope so the Python wrapper
    /// can confirm the binary supports the long-running mode before sending
    /// any requests. Wave 8-A of #348.
    #[command(hide = true)]
    TranscribeServer,
    /// Phase 2.1 cross-platform injection: reads a JSON request envelope on
    /// stdin and writes a JSON response on stdout. Gated at runtime by
    /// VOICEPI_INJECTION_BACKEND=rust (the Python worker decides whether to
    /// shell out). Hidden because it's a worker-only RPC.
    #[command(hide = true)]
    Inject,
    /// Enumerate input audio devices as JSON. Built into binaries with the
    /// `audio-in-rust` feature; binaries without the feature print a structured
    /// error and exit non-zero so the Python caller can fall back to its own
    /// path. Used by `vp_devices.py` when `VOICEPI_DEVICES_BACKEND=rust`.
    Devices,
    /// Internal helper used by the single-instance gate integration
    /// tests (issue #327). NOT a supported user-facing entry point:
    /// the flags and output format may change without notice.
    ///
    /// Two modes:
    ///
    /// * `--serve-ms N`: act as the running instance. Acquire the
    ///   single-instance lock, listen for up to `N` milliseconds,
    ///   print any forwarded argv as `[forwarded] <json>` lines to
    ///   stdout, then release the lock and exit 0.
    /// * default: act as the client. Attempt to forward `forward_args`
    ///   to a running instance. Prints `[forwarded]` on success or
    ///   `[acquired]` if there was no running instance (in which case
    ///   the lock is released immediately and the process exits).
    ///
    /// Combined with `VOICEPI_SINGLE_INSTANCE_DIR=<tempdir>` this lets
    /// the integration test drive two real processes without stomping
    /// on a live user daemon.
    #[command(hide = true)]
    SingleInstanceProbe {
        /// Serve mode: hold the lock for this many milliseconds while
        /// printing forwarded argv to stdout.
        #[arg(long)]
        serve_ms: Option<u64>,
        /// argv the client mode forwards to the running instance.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        forward_args: Vec<String>,
    },
    /// Wave 5 PR 6 of #348: long-running in-process Rust dictation worker.
    /// Replaces the Python `vp_dictate.py`/`runtime.py` orchestrator when
    /// `VOICEPI_DICTATE_BACKEND=rust-session` is set AND the binary was built
    /// with `--features whisper-rs-local,rust-injection,audio-in-rust,rust-hotkeys`.
    /// Without the env var or the features the supervisor stays on the
    /// Python path (PR 7 will flip the default).
    ///
    /// Owns the full PTT lifecycle in-process: installs the Rust hotkey
    /// listener (rdev), spawns the audio pump (cpal -> Silero VAD), drives
    /// the [`crate::dictate::DictateSession`] from the hotkey coordinator,
    /// and emits `[worker-event] {...}` lines on stderr.
    ///
    /// Always reads stdin: a line of `press`, `release`, `cancel`, or
    /// `quit` drives the coordinator (or shuts the worker down). Used by
    /// the integration test to drive synthetic chord events without a
    /// real OS listener, and gives the supervisor a clean shutdown path
    /// (close stdin → worker exits) without relying on signals on Windows.
    ///
    /// Hidden because it is invoked only by the supervisor and tests, not
    /// directly by users.
    #[command(hide = true)]
    WorkerRust {
        /// Skip installing the real rdev hotkey listener; drive the
        /// coordinator from stdin commands only. Used by the integration
        /// test in headless CI environments where no display is available
        /// for rdev to attach to. Production callers leave this unset so
        /// the OS hotkey listener takes over the PTT chord.
        #[arg(long)]
        stdin_only: bool,
    },
    /// Manage local Whisper model files (catalog, download, verify).
    ///
    /// Backwards compatibility: `VOICEPI_WHISPER_MODEL_PATH` still wins for the
    /// inference path; this subcommand only manages files in the
    /// `whisper-models/` user-cache directory. See `whisper::model_manager`.
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ModelsCommand {
    /// List the curated download catalog with each entry's name, file size,
    /// description, and whether a verified copy is already in the user cache.
    List,
    /// Download a catalog entry into the user cache. Verifies SHA-256 after
    /// the download completes; on mismatch the partial file is deleted.
    Download {
        /// Catalog name (e.g. `tiny.en`, `base.en`, `small.en`). Use
        /// `models list` to see what's available.
        name: String,
    },
    /// Print the cache directory where downloaded models are stored.
    Path,
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
    /// Extract domain terms from the golden-corpus reference TEXT (curated
    /// terms + capitalised/multi-word/technical tokens) and append+dedup them
    /// into the dictionary. PREVIEW by default; pass `--apply` to write.
    /// Reads corpus TEXT only — it never records or touches audio. Honours
    /// `--language` / `--category` for profile selection.
    #[command(name = "build-from-corpus")]
    BuildFromCorpus {
        /// Path to the corpus manifest (`benchmark/corpus.json`). When omitted
        /// the resolver searches `<app_root>/benchmark/corpus.json` and the
        /// per-user appdata equivalent.
        #[arg(long = "benchmark-corpus", value_name = "PATH")]
        benchmark_corpus: Option<String>,
        /// Override the app root used to resolve the corpus manifest. Hidden
        /// because it mirrors the Python `--app-root` test helper.
        #[arg(long, hide = true)]
        app_root: Option<String>,
        /// Dictionary file to read / append. Default: `$VOICEPI_DICTIONARY` or
        /// the per-user `dictionary.json`.
        #[arg(long, value_name = "PATH")]
        dictionary: Option<String>,
        /// Profile selector: restrict to these languages (e.g. `da` or
        /// `da,en`).
        #[arg(long)]
        language: Option<String>,
        /// Profile selector: restrict to these categories or friendly groups
        /// (e.g. `technical`, `names`, `mixed_technical`).
        #[arg(long)]
        category: Option<String>,
        /// Minimum corpus occurrence count for a term to be proposed (default
        /// 1).
        #[arg(long, default_value_t = 1)]
        min_count: usize,
        /// WRITE the changes instead of only previewing them.
        #[arg(long)]
        apply: bool,
        /// Emit machine-readable JSON to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Read an annotated benchmark JSONL and SUGGEST the domain terms the
    /// model missed (`term_misses`) as dictionary additions. PREVIEW by
    /// default; pass `--apply` to add the new terms. Reads result TEXT only —
    /// never records audio.
    #[command(name = "suggest-terms")]
    SuggestTerms {
        /// Path to the annotated benchmark JSONL emitted by `--run-benchmark`.
        jsonl: String,
        /// Dictionary file to read / append. Default: `$VOICEPI_DICTIONARY` or
        /// the per-user `dictionary.json`.
        #[arg(long, value_name = "PATH")]
        dictionary: Option<String>,
        /// Minimum miss count for a term to be suggested (default 1).
        #[arg(long, default_value_t = 1)]
        min_count: usize,
        /// WRITE the new terms instead of only previewing them.
        #[arg(long)]
        apply: bool,
        /// Emit machine-readable JSON to stdout.
        #[arg(long)]
        json: bool,
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
    /// Substring-search the SQLite history store (issue #324). Returns
    /// rows whose text matches `query` via FTS5 (when available) or
    /// LIKE. Requires a binary built with the default `history-sqlite`
    /// feature; otherwise the subcommand prints a friendly hint and
    /// exits non-zero.
    Search {
        /// Search query — words are matched as a phrase. Pass an
        /// empty string to scan recent rows without a text filter.
        query: String,
        /// Maximum rows to return.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Skip this many leading rows (pagination offset).
        #[arg(long, default_value_t = 0)]
        offset: u32,
    },
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
        assert_eq!(cli.command, Some(Command::ModelCapacity { json: false }));

        let cli = Cli::parse_from(["whisper-dictate", "model-capacity", "--json"]);
        assert_eq!(cli.command, Some(Command::ModelCapacity { json: true }));
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
    fn parses_dictionary_build_from_corpus_with_flags() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "dictionary",
            "build-from-corpus",
            "--benchmark-corpus",
            "corpus.json",
            "--language",
            "da",
            "--category",
            "technical",
            "--dictionary",
            "d.json",
            "--apply",
            "--min-count",
            "2",
            "--json",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::Dictionary {
                command: DictionaryCommand::BuildFromCorpus {
                    benchmark_corpus: Some("corpus.json".to_owned()),
                    app_root: None,
                    dictionary: Some("d.json".to_owned()),
                    language: Some("da".to_owned()),
                    category: Some("technical".to_owned()),
                    min_count: 2,
                    apply: true,
                    json: true,
                },
            })
        );
    }

    #[test]
    fn parses_dictionary_build_from_corpus_with_defaults() {
        // No flags besides the subcommand: every option falls back to its
        // Python-default counterpart (no corpus override, no dict override,
        // min_count=1, preview-only, no JSON).
        let cli = Cli::parse_from(["whisper-dictate", "dictionary", "build-from-corpus"]);
        assert_eq!(
            cli.command,
            Some(Command::Dictionary {
                command: DictionaryCommand::BuildFromCorpus {
                    benchmark_corpus: None,
                    app_root: None,
                    dictionary: None,
                    language: None,
                    category: None,
                    min_count: 1,
                    apply: false,
                    json: false,
                },
            })
        );
    }

    #[test]
    fn parses_dictionary_suggest_terms_with_flags() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "dictionary",
            "suggest-terms",
            "results.jsonl",
            "--dictionary",
            "d.json",
            "--apply",
            "--json",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::Dictionary {
                command: DictionaryCommand::SuggestTerms {
                    jsonl: "results.jsonl".to_owned(),
                    dictionary: Some("d.json".to_owned()),
                    min_count: 1,
                    apply: true,
                    json: true,
                },
            })
        );
    }

    #[test]
    fn parses_hidden_dictionary_runtime_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "dictionary-runtime"]);
        assert_eq!(cli.command, Some(Command::DictionaryRuntime));
    }

    #[test]
    fn parses_hidden_dictionary_ops_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "dictionary-ops"]);
        assert_eq!(cli.command, Some(Command::DictionaryOps));
    }

    #[test]
    fn parses_hidden_dictate_ops_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "dictate-ops"]);
        assert_eq!(cli.command, Some(Command::DictateOps));
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
    fn parses_history_search_subcommand_with_defaults() {
        let cli = Cli::parse_from(["whisper-dictate", "history", "search", "rødgrød"]);
        assert_eq!(
            cli.command,
            Some(Command::History {
                command: HistoryCommand::Search {
                    query: "rødgrød".to_owned(),
                    limit: 20,
                    offset: 0,
                },
            })
        );
    }

    #[test]
    fn parses_history_search_subcommand_with_pagination_flags() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "history",
            "search",
            "needle",
            "--limit",
            "5",
            "--offset",
            "10",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::History {
                command: HistoryCommand::Search {
                    query: "needle".to_owned(),
                    limit: 5,
                    offset: 10,
                },
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

    #[test]
    fn parses_hidden_telemetry_helpers() {
        let cli = Cli::parse_from(["whisper-dictate", "append-jsonl", "--path", "metrics.jsonl"]);
        assert_eq!(
            cli.command,
            Some(Command::AppendJsonl {
                path: "metrics.jsonl".to_owned(),
            })
        );

        let cli = Cli::parse_from([
            "whisper-dictate",
            "append-history",
            "--path",
            "history.jsonl",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::AppendHistory {
                path: "history.jsonl".to_owned(),
            })
        );

        let cli = Cli::parse_from(["whisper-dictate", "append-record-sinks"]);
        assert_eq!(cli.command, Some(Command::AppendRecordSinks));

        let cli = Cli::parse_from(["whisper-dictate", "worker-event"]);
        assert_eq!(cli.command, Some(Command::WorkerEvent));

        let cli = Cli::parse_from(["whisper-dictate", "command-hook"]);
        assert_eq!(cli.command, Some(Command::CommandHook));

        let cli = Cli::parse_from(["whisper-dictate", "redact-text"]);
        assert_eq!(cli.command, Some(Command::RedactText));

        let cli = Cli::parse_from(["whisper-dictate", "apply-profile"]);
        assert_eq!(cli.command, Some(Command::ApplyProfile));

        let cli = Cli::parse_from(["whisper-dictate", "privacy"]);
        assert_eq!(cli.command, Some(Command::Privacy));

        let cli = Cli::parse_from(["whisper-dictate", "postprocess"]);
        assert_eq!(cli.command, Some(Command::Postprocess));

        let cli = Cli::parse_from(["whisper-dictate", "external-api"]);
        assert_eq!(cli.command, Some(Command::ExternalApi));

        let cli = Cli::parse_from(["whisper-dictate", "health"]);
        assert_eq!(cli.command, Some(Command::Health));

        let cli = Cli::parse_from(["whisper-dictate", "transcribe-wav"]);
        assert_eq!(cli.command, Some(Command::TranscribeWav { probe: false }));

        let cli = Cli::parse_from(["whisper-dictate", "transcribe-wav", "--probe"]);
        assert_eq!(cli.command, Some(Command::TranscribeWav { probe: true }));

        // Wave 8-A: the long-running in-process worker subcommand.
        let cli = Cli::parse_from(["whisper-dictate", "transcribe-server"]);
        assert_eq!(cli.command, Some(Command::TranscribeServer));
    }

    #[test]
    fn parses_hidden_inject_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "inject"]);
        assert_eq!(cli.command, Some(Command::Inject));
    }

    #[test]
    fn parses_devices_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "devices"]);
        assert_eq!(cli.command, Some(Command::Devices));
    }

    #[test]
    fn parses_bench_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "bench"]);
        assert_eq!(cli.command, Some(Command::Bench));
    }

    #[test]
    fn parses_benchmark_alias_subcommand() {
        // `benchmark` is exposed as an alias of `bench` so users (and the
        // older docs) can spell it out without the parser tripping.
        let cli = Cli::parse_from(["whisper-dictate", "benchmark"]);
        assert_eq!(cli.command, Some(Command::Bench));
    }

    #[test]
    fn parses_corpus_record_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "corpus-record", "da-001"]);
        assert_eq!(
            cli.command,
            Some(Command::CorpusRecord {
                id: "da-001".to_owned(),
            })
        );
    }

    #[test]
    fn parses_models_list_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "models", "list"]);
        assert_eq!(
            cli.command,
            Some(Command::Models {
                command: ModelsCommand::List,
            })
        );
    }

    #[test]
    fn parses_models_download_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "models", "download", "tiny.en"]);
        assert_eq!(
            cli.command,
            Some(Command::Models {
                command: ModelsCommand::Download {
                    name: "tiny.en".to_owned(),
                },
            })
        );
    }

    #[test]
    fn parses_models_path_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "models", "path"]);
        assert_eq!(
            cli.command,
            Some(Command::Models {
                command: ModelsCommand::Path,
            })
        );
    }

    // --- External toggle flags (issue #326) -------------------------------

    #[test]
    fn parses_toggle_recording_flag() {
        use crate::runtime::external_toggle::ExternalCommand;
        let cli = Cli::parse_from(["whisper-dictate", "--toggle-recording"]);
        assert!(cli.toggle_recording);
        assert_eq!(cli.external_command(), Some(ExternalCommand::Toggle));
    }

    #[test]
    fn parses_start_recording_flag() {
        use crate::runtime::external_toggle::ExternalCommand;
        let cli = Cli::parse_from(["whisper-dictate", "--start-recording"]);
        assert!(cli.start_recording);
        assert_eq!(cli.external_command(), Some(ExternalCommand::Start));
    }

    #[test]
    fn parses_stop_recording_flag() {
        use crate::runtime::external_toggle::ExternalCommand;
        let cli = Cli::parse_from(["whisper-dictate", "--stop-recording"]);
        assert!(cli.stop_recording);
        assert_eq!(cli.external_command(), Some(ExternalCommand::Stop));
    }

    #[test]
    fn parses_cancel_recording_flag() {
        use crate::runtime::external_toggle::ExternalCommand;
        let cli = Cli::parse_from(["whisper-dictate", "--cancel-recording"]);
        assert!(cli.cancel_recording);
        assert_eq!(cli.external_command(), Some(ExternalCommand::Cancel));
    }

    #[test]
    fn external_command_is_none_when_no_flag_set() {
        let cli = Cli::parse_from(["whisper-dictate"]);
        assert!(cli.external_command().is_none());
    }

    #[test]
    fn external_toggle_flags_conflict_with_each_other() {
        // clap enforces the conflicts_with_all attribute: invoking two
        // mutually-exclusive flags must fail at parse time so we never
        // silently pick one over the other.
        let err = Cli::try_parse_from([
            "whisper-dictate",
            "--toggle-recording",
            "--cancel-recording",
        ])
        .expect_err("conflicting flags must error");
        assert!(
            err.to_string()
                .to_ascii_lowercase()
                .contains("cannot be used with"),
            "expected clap conflict error, got: {err}"
        );
    }

    #[test]
    fn external_toggle_flag_takes_precedence_over_subcommand() {
        // Clap does not expose the `#[command(subcommand)]` field as a valid
        // `conflicts_with` target, so a user that types
        // `whisper-dictate --toggle-recording ui` parses without a parse-time
        // error. `main.rs` short-circuits to the IPC forwarder when
        // `external_command()` is `Some`, so the subcommand is silently
        // ignored — assert the flag wins, documenting the precedence
        // contract.
        let cli = Cli::parse_from(["whisper-dictate", "--toggle-recording", "ui"]);
        assert!(cli.toggle_recording);
        assert_eq!(cli.command, Some(Command::Ui));
        assert!(cli.external_command().is_some());
    }

    #[test]
    fn parses_worker_rust_subcommand_default() {
        // Wave 5 PR 6 of #348: the new long-running Rust dictation worker
        // subcommand. Defaults `stdin_only=false` so production builds
        // install the real rdev listener (the integration test sets the
        // flag to skip it on headless CI).
        let cli = Cli::parse_from(["whisper-dictate", "worker-rust"]);
        assert_eq!(cli.command, Some(Command::WorkerRust { stdin_only: false }),);
    }

    #[test]
    fn parses_worker_rust_with_stdin_only_flag() {
        let cli = Cli::parse_from(["whisper-dictate", "worker-rust", "--stdin-only"]);
        assert_eq!(cli.command, Some(Command::WorkerRust { stdin_only: true }));
    }

    #[test]
    fn corpus_record_accepts_hyphen_leading_id() {
        // Regression for the Codex P3 finding on PR #360: a manifest item whose
        // safe id starts with `-` (e.g. `-sample`) must reach the validator
        // rather than getting rejected by clap as a stray flag. The full
        // `[A-Za-z0-9._-]+` allowlist is then enforced by
        // `corpus_record::is_safe_corpus_id` BEFORE shelling out.
        let cli = Cli::parse_from(["whisper-dictate", "corpus-record", "-sample"]);
        assert_eq!(
            cli.command,
            Some(Command::CorpusRecord {
                id: "-sample".to_owned(),
            })
        );
    }
}
