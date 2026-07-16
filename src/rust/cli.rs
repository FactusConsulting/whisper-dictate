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
    /// Library-first POC: drive the full push-to-talk pipeline
    /// (transcribe → dictionary → post-process → inject) against a WAV
    /// file, without opening a microphone or installing a keyboard hook.
    /// Defaults to a dry-run that prints the would-be-typed transcript;
    /// pass `--inject` to really type into the active window.
    ///
    /// Forwards to the Python worker's `--simulate-ptt` flag so the same
    /// pipeline the live PTT loop drives is exercised end to end. Intended
    /// for headless verification in a build container (e.g. ubuntu:26.04)
    /// where no audio hardware or WM keyboard hook is available.
    SimulatePtt {
        /// WAV/audio file to transcribe.
        #[arg(long)]
        wav: String,
        /// Really invoke the injection backend (default: dry-run).
        /// Only the direct-typing strategy is implemented in this POC —
        /// a headless paste strategy is future work, so no `--inject-mode`
        /// selector is exposed until it lands.
        #[arg(long, default_value_t = false)]
        inject: bool,
        /// Spoken-language hint (`da`, `en`, ...); omit to let Whisper
        /// auto-detect.
        #[arg(long, default_value = "")]
        language: String,
        /// Whisper model name (default: read from config / `VOICEPI_MODEL`).
        #[arg(long, default_value = "")]
        model: String,
        /// Emit the result as a single JSON line instead of the transcript.
        #[arg(long, default_value_t = false)]
        json: bool,
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
    /// Diagnostic tools for the push-to-talk hotkey listener.
    ///
    /// `hotkey capture` installs the listener for a bounded window and prints
    /// every OS key event plus every chord match/release the coordinator sees.
    /// Intended for debugging PTT wedges ("does the listener see my chord?")
    /// and as a headless smoke test that proves the install path works on the
    /// running platform without opening the full dictation runtime.
    Hotkey {
        #[command(subcommand)]
        command: HotkeyCommand,
    },
    /// Inject text into the active window — scripting + smoke-test wrapper
    /// around the injection library. **Defaults to `--dry-run`**: the
    /// resolved backend + keystroke plan is printed and NOTHING is typed.
    /// Real injection requires `--do-it` (alias `--live`).
    ///
    /// Two invocation shapes coexist:
    ///
    /// * `inject-text <TEXT> [--dry-run|--do-it] [--backend NAME] [--json]`
    ///   — the public scripting/smoke form (audit item 2 chunk B).
    /// * `inject-text --mode {type|paste} --text ... --xkb-layout ...
    ///   --target-title ... --target-process ...` — the legacy hidden
    ///   helper that the Python worker still shells out to for Wayland
    ///   layout-aware typing. Keep working for backwards-compat; the
    ///   public path never sets `--mode`.
    InjectText {
        /// Text to inject (public form — positional). When present the
        /// command runs the dry-run/inject flow; when absent the legacy
        /// `--mode` + `--text` helper path runs.
        #[arg(value_name = "TEXT")]
        text_arg: Option<String>,
        /// Explicit dry-run flag (matches the default). Set for clarity /
        /// self-documenting shell scripts; `--do-it` overrides.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// REALLY inject the text into the active window (dangerous —
        /// moves the cursor, types keys). Off by default. `--live` is an
        /// alias for the same flag.
        #[arg(long, alias = "live", default_value_t = false)]
        do_it: bool,
        /// Backend selector for the public form. `auto` picks per platform
        /// via [`plan::pick_backend`]; explicit backends are echoed back;
        /// `type` / `paste` are MODE aliases (not backend names).
        #[arg(
            long,
            default_value = "auto",
            value_parser = [
                "auto", "pynput", "wtype", "ydotool", "xdotool", "kwtype",
                "dotool", "enigo", "type", "paste",
            ],
        )]
        backend: String,
        /// Machine-readable JSON output (single line). Default is a
        /// human-readable summary — the JSON keys are stable so tests can
        /// pin them.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Legacy hidden-helper mode selector (`type` / `paste`). Non-empty
        /// value selects the legacy Wayland keycode path used by the Python
        /// worker; leave empty for the public dry-run form.
        #[arg(long, default_value = "", value_parser = ["", "type", "paste"])]
        mode: String,
        /// Legacy hidden-helper text (used when `--mode` is set). The
        /// public form uses the positional TEXT argument.
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
    /// Enumerate input audio devices as JSON, or dry-run test a specific
    /// microphone. With no subcommand the binary reads a JSON envelope on
    /// stdin (`{"action":"list"|"default"|"find","query":"..."}`) and prints
    /// the matching JSON response — this is the hidden helper `vp_devices.py`
    /// shells out to when `VOICEPI_DEVICES_BACKEND=rust`. Built into binaries
    /// with the `audio-in-rust` feature; binaries without the feature print
    /// a structured error and exit non-zero so the Python caller can fall
    /// back to its own path.
    ///
    /// The `test <NAME>` subcommand shells out to the Python worker's
    /// `--test-audio-device` query mode (which reuses the same live-capture
    /// WASAPI/DirectSound/MME open matrix), prints a single JSON usability
    /// result to stdout and exits — no ML model is loaded. Pass an empty
    /// string to test the system default input. Enables headless mic
    /// verification in the CI container (audit item 3 —
    /// `docs/architecture-audit-2026-07-16.md`).
    Devices {
        #[command(subcommand)]
        command: Option<DevicesCommand>,
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
    /// Print the current value of a single setting key.
    ///
    /// The key must be one of the settings owned by the typed `AppSettings`
    /// (`stt_backend`, `audio_device`, `model`, …). Unknown keys exit 1 with
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
    /// UI uses on save — invalid enum values or unparseable numbers fail
    /// cleanly WITHOUT touching the file on disk. Booleans accept
    /// `1`/`0`/`true`/`false`/`yes`/`no`/`on`/`off` (case-insensitive) and
    /// are normalised to the `"1"`/`"0"` form the worker expects.
    Set {
        /// Setting key (e.g. `audio_device`, `model`, `stt_backend`).
        key: String,
        /// New value for `key`. Empty string clears the setting (equivalent
        /// to removing the key from the config file — the worker will fall
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
        /// Emit machine-readable JSON: an object of `{key: value, …}`.
        #[arg(long)]
        json: bool,
        /// Override the config file path (default: platform user config).
        #[arg(long, value_name = "PATH")]
        config: Option<String>,
    },
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
    /// Print the Whisper `initial_prompt` string that would be sent for
    /// dictations against the current dictionary + config. Useful for
    /// eyeballing whether a term / replacement list produces a sane prompt
    /// without spinning up the Python worker.
    ///
    /// Reads the dictionary at `--dictionary` (or `$VOICEPI_DICTIONARY`, or
    /// the per-user default) and the base prompt from `config.json`
    /// (`initial_prompt`). `--max-length` overrides the character cap so
    /// you can inspect how a smaller / larger budget would trim the
    /// vocabulary list. `--json` emits a machine-readable payload with
    /// `prompt`, `length_chars`, `term_count`, `truncated`, `source`.
    #[command(name = "prompt")]
    Prompt {
        /// Dictionary file to read. Default: `$VOICEPI_DICTIONARY` or the
        /// per-user `dictionary.json`. A missing file is treated as an
        /// empty dictionary (fresh installs before the first term is
        /// added).
        #[arg(long, value_name = "PATH")]
        dictionary: Option<String>,
        /// Emit machine-readable JSON to stdout.
        #[arg(long)]
        json: bool,
        /// Override the prompt character cap (default: config
        /// `dictionary_prompt_chars`, typically 1200). Passing a small
        /// value here is a fast way to preview how term-list truncation
        /// would land.
        #[arg(long, value_name = "N")]
        max_length: Option<usize>,
    },
    /// Print the raw terms + replacements the runtime loaded from the
    /// dictionary. Bonus adapter around the same loader `prompt` uses —
    /// no network, no Python.
    #[command(name = "list")]
    List {
        /// Dictionary file to read. Default: `$VOICEPI_DICTIONARY` or the
        /// per-user `dictionary.json`.
        #[arg(long, value_name = "PATH")]
        dictionary: Option<String>,
        /// Emit machine-readable JSON to stdout.
        #[arg(long)]
        json: bool,
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
pub enum DevicesCommand {
    /// Dry-run open the named microphone and print a single JSON usability
    /// result. Resolves `name` against the live device list (case-insensitive
    /// substring — same rule the picker uses), then tries the same
    /// WASAPI/DirectSound/MME open matrix as live capture (opening and
    /// immediately closing each candidate, capturing no audio). Loads no ML
    /// model. Pass an empty string to test the system default input.
    ///
    /// Output shape (single JSON object, one line):
    /// `{"device":"...", "usable":true|false, "endpoint":"wasapi|directsound|mme|default|null", "samplerate":<int|null>, "dtype":"...|null", "resampled":true|false, "reason":"...|null"}`.
    /// A non-usable device still exits 0 — it's a normal reportable outcome,
    /// not a CLI error. Only sub-process launch failures return non-zero.
    Test {
        /// Microphone name (case-insensitive substring; same matching rule as
        /// the picker). Pass `""` to test the system default input.
        ///
        /// `allow_hyphen_values` lets rare device names that start with `-`
        /// through clap so the Python resolver can decide, rather than clap
        /// rejecting the value as a stray flag.
        #[arg(allow_hyphen_values = true)]
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum HistoryCommand {
    /// List recent history rows (human-readable summary tail).
    List {
        /// Number of rows to show.
        #[arg(default_value_t = 10)]
        limit: usize,
    },
    /// Print the most recent N transcripts (newest first). By default emits
    /// only the `text` field, one entry per line — pass `--json` to get the
    /// full entry objects as a JSON array.
    Last {
        /// Number of transcripts to print (default 1). Values <=0 are
        /// clamped to 1 so scripts can pass through user input safely.
        #[arg(long, default_value_t = 1)]
        n: usize,
        /// Emit a JSON array of the entries instead of plain text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Copy the most recent transcript to the system clipboard. Uses the
    /// first available OS backend (`wl-copy` / `xclip` on Linux, `clip.exe`
    /// on Windows, `pbcopy` on macOS). Prints `copied: <text>` on success.
    /// Exits non-zero when the history is empty or no clipboard tool is
    /// available.
    #[command(name = "copy-last")]
    CopyLast,
    /// Feed the last transcript back into the injection pipeline. Wraps
    /// `inject-text` — same backend selection and dry-run guardrails — so
    /// this verb **defaults to a dry-run**: it prints the plan and does
    /// nothing else. Pass `--do-it` (alias `--live`) to actually type.
    #[command(name = "reinject-last")]
    ReinjectLast {
        /// Explicit dry-run flag (matches the default). Set for clarity /
        /// self-documenting shell scripts; `--do-it` overrides.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// REALLY inject the text into the active window (dangerous —
        /// moves the cursor, types keys). Off by default.
        #[arg(long, alias = "live", default_value_t = false)]
        do_it: bool,
        /// Machine-readable JSON output (single line) of the injection plan.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Backend selector; forwarded to `inject-text`. Default `auto`.
        #[arg(
            long,
            default_value = "auto",
            value_parser = [
                "auto", "pynput", "wtype", "ydotool", "xdotool", "kwtype",
                "dotool", "enigo", "type", "paste",
            ],
        )]
        backend: String,
    },
    /// Substring search over transcripts (case-insensitive). Newest matches
    /// first, up to `--limit` (default 20). JSON output is a JSON array of
    /// full entries.
    Search {
        /// Substring to search for in the `text` field.
        query: String,
        /// Cap on the number of matches returned. Values <=0 clamp to 1.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Emit a JSON array of the matching entries instead of plain text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum HotkeyCommand {
    /// Install the PTT listener for a bounded window and print every OS key
    /// event and chord match/release it observes. Exits 0 when the window
    /// elapses (or immediately when `--exit-on-chord` is set and the
    /// configured chord fires). A listener startup failure (no display,
    /// missing accessibility permission, unsupported chord) exits non-zero
    /// with a clear message so smoke scripts can distinguish "listener
    /// unavailable on this platform" from a genuine regression.
    ///
    /// Output line prefix is `[hotkey-capture]`. `--json` switches to one
    /// JSON object per line (JSONL): `{"t":<seconds>,"kind":"...","...":...}`.
    /// The plain-text format is stable-ish (line prefix + kind tokens) but
    /// the JSON keys are the contract callers should pin against.
    Capture {
        /// Duration in seconds to keep the listener installed. Fractional
        /// values are allowed so smoke scripts can use a sub-second window
        /// (e.g. `--for 0.5`). Parsed as `f64` in the handler; the string
        /// carrier keeps the enum `Eq`-derivable so parse-shape tests can
        /// still `assert_eq!` variants without a bespoke matcher.
        #[arg(long = "for", value_name = "SECONDS", default_value = "5")]
        for_secs: String,
        /// Emit machine-readable JSONL instead of the human-readable
        /// `[hotkey-capture] ...` lines.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Exit 0 as soon as the configured PTT chord fires. Useful for CI
        /// smoke tests where a driven synthetic press proves the whole path.
        #[arg(long = "exit-on-chord", default_value_t = false)]
        exit_on_chord: bool,
        /// Override the config file path used to look up the PTT chord.
        /// Default: platform user config (honours `VOICEPI_CONFIG` when this
        /// flag is omitted).
        #[arg(long, value_name = "PATH")]
        config: Option<String>,
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
    fn parses_config_get_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "config", "get", "audio_device"]);
        assert_eq!(
            cli.command,
            Some(Command::Config {
                command: ConfigCommand::Get {
                    key: "audio_device".to_owned(),
                    json: false,
                    config: None,
                },
            })
        );

        let cli = Cli::parse_from([
            "whisper-dictate",
            "config",
            "get",
            "model",
            "--json",
            "--config",
            "/tmp/cfg.json",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::Config {
                command: ConfigCommand::Get {
                    key: "model".to_owned(),
                    json: true,
                    config: Some("/tmp/cfg.json".to_owned()),
                },
            })
        );
    }

    #[test]
    fn parses_config_set_subcommand() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "config",
            "set",
            "model",
            "large-v3-turbo",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::Config {
                command: ConfigCommand::Set {
                    key: "model".to_owned(),
                    value: "large-v3-turbo".to_owned(),
                    config: None,
                },
            })
        );
    }

    #[test]
    fn parses_config_set_accepts_hyphen_leading_value() {
        // A device name or a negative dBFS may start with `-`; clap must let
        // it through so the settings validator (not clap) is the one that
        // sees the raw value.
        let cli = Cli::parse_from(["whisper-dictate", "config", "set", "target_dbfs", "-24"]);
        assert_eq!(
            cli.command,
            Some(Command::Config {
                command: ConfigCommand::Set {
                    key: "target_dbfs".to_owned(),
                    value: "-24".to_owned(),
                    config: None,
                },
            })
        );
    }

    #[test]
    fn parses_config_list_subcommand() {
        let cli = Cli::parse_from(["whisper-dictate", "config", "list"]);
        assert_eq!(
            cli.command,
            Some(Command::Config {
                command: ConfigCommand::List {
                    json: false,
                    config: None,
                },
            })
        );

        let cli = Cli::parse_from(["whisper-dictate", "config", "list", "--json"]);
        assert_eq!(
            cli.command,
            Some(Command::Config {
                command: ConfigCommand::List {
                    json: true,
                    config: None,
                },
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
    fn parses_dictionary_prompt_defaults() {
        let cli = Cli::parse_from(["whisper-dictate", "dictionary", "prompt"]);
        assert_eq!(
            cli.command,
            Some(Command::Dictionary {
                command: DictionaryCommand::Prompt {
                    dictionary: None,
                    json: false,
                    max_length: None,
                },
            })
        );
    }

    #[test]
    fn parses_dictionary_prompt_with_all_flags() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "dictionary",
            "prompt",
            "--dictionary",
            "d.json",
            "--json",
            "--max-length",
            "256",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::Dictionary {
                command: DictionaryCommand::Prompt {
                    dictionary: Some("d.json".to_owned()),
                    json: true,
                    max_length: Some(256),
                },
            })
        );
    }

    #[test]
    fn parses_dictionary_list_defaults() {
        let cli = Cli::parse_from(["whisper-dictate", "dictionary", "list"]);
        assert_eq!(
            cli.command,
            Some(Command::Dictionary {
                command: DictionaryCommand::List {
                    dictionary: None,
                    json: false,
                },
            })
        );
    }

    #[test]
    fn parses_dictionary_list_with_flags() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "dictionary",
            "list",
            "--dictionary",
            "d.json",
            "--json",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::Dictionary {
                command: DictionaryCommand::List {
                    dictionary: Some("d.json".to_owned()),
                    json: true,
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
    fn parses_history_last_defaults_n_1_no_json() {
        // Bare `history last` must still parse — the flag additions must
        // remain backward-compatible with the shipping invocation.
        let cli = Cli::parse_from(["whisper-dictate", "history", "last"]);
        assert_eq!(
            cli.command,
            Some(Command::History {
                command: HistoryCommand::Last { n: 1, json: false },
            })
        );
    }

    #[test]
    fn parses_history_last_with_flags() {
        let cli = Cli::parse_from(["whisper-dictate", "history", "last", "--n", "5", "--json"]);
        assert_eq!(
            cli.command,
            Some(Command::History {
                command: HistoryCommand::Last { n: 5, json: true },
            })
        );
    }

    #[test]
    fn parses_history_copy_last() {
        let cli = Cli::parse_from(["whisper-dictate", "history", "copy-last"]);
        assert_eq!(
            cli.command,
            Some(Command::History {
                command: HistoryCommand::CopyLast,
            })
        );
    }

    #[test]
    fn parses_history_reinject_last_defaults() {
        // Bare `history reinject-last` MUST default to safe (dry_run=false,
        // do_it=false — handler treats as dry-run).
        let cli = Cli::parse_from(["whisper-dictate", "history", "reinject-last"]);
        assert_eq!(
            cli.command,
            Some(Command::History {
                command: HistoryCommand::ReinjectLast {
                    dry_run: false,
                    do_it: false,
                    json: false,
                    backend: "auto".to_owned(),
                },
            })
        );
    }

    #[test]
    fn parses_history_reinject_last_do_it_json() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "history",
            "reinject-last",
            "--do-it",
            "--json",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::History {
                command: HistoryCommand::ReinjectLast {
                    dry_run: false,
                    do_it: true,
                    json: true,
                    backend: "auto".to_owned(),
                },
            })
        );
    }

    #[test]
    fn parses_history_search_with_flags() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "history",
            "search",
            "codex",
            "--limit",
            "5",
            "--json",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::History {
                command: HistoryCommand::Search {
                    query: "codex".to_owned(),
                    limit: 5,
                    json: true,
                },
            })
        );
    }

    #[test]
    fn parses_legacy_hidden_inject_text_helper_flags() {
        // Backwards-compat: the Python worker still shells out to
        // `inject-text --mode type --text ... --xkb-layout ...` — that
        // invocation MUST keep parsing exactly as before even though the
        // command now grows public-form flags.
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
                text_arg: None,
                dry_run: false,
                do_it: false,
                backend: "auto".to_owned(),
                json: false,
                mode: "type".to_owned(),
                text: "høre".to_owned(),
                xkb_layout: "dk".to_owned(),
                target_title: String::new(),
                target_process: String::new(),
            })
        );
    }

    #[test]
    fn parses_public_inject_text_positional_defaults_to_dry_run() {
        // Public form: positional TEXT + implicit dry-run (no --do-it).
        let cli = Cli::parse_from(["whisper-dictate", "inject-text", "smoke test"]);
        assert_eq!(
            cli.command,
            Some(Command::InjectText {
                text_arg: Some("smoke test".to_owned()),
                dry_run: false, // flag not passed; handler still treats as dry-run
                do_it: false,
                backend: "auto".to_owned(),
                json: false,
                mode: String::new(),
                text: String::new(),
                xkb_layout: String::new(),
                target_title: String::new(),
                target_process: String::new(),
            })
        );
    }

    #[test]
    fn parses_public_inject_text_with_explicit_dry_run_and_json_and_backend() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "inject-text",
            "hej",
            "--dry-run",
            "--backend",
            "wtype",
            "--json",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::InjectText {
                text_arg: Some("hej".to_owned()),
                dry_run: true,
                do_it: false,
                backend: "wtype".to_owned(),
                json: true,
                mode: String::new(),
                text: String::new(),
                xkb_layout: String::new(),
                target_title: String::new(),
                target_process: String::new(),
            })
        );
    }

    #[test]
    fn parses_public_inject_text_do_it_and_live_alias() {
        // `--do-it` opts into real injection.
        let cli = Cli::parse_from(["whisper-dictate", "inject-text", "hi", "--do-it"]);
        assert!(matches!(
            cli.command,
            Some(Command::InjectText { do_it: true, .. })
        ));
        // `--live` is an alias for the same flag — same parsed state.
        let cli = Cli::parse_from(["whisper-dictate", "inject-text", "hi", "--live"]);
        assert!(matches!(
            cli.command,
            Some(Command::InjectText { do_it: true, .. })
        ));
    }

    #[test]
    fn rejects_unknown_backend_at_parse_time() {
        // The value_parser allowlist stops typos before the handler runs.
        let err = Cli::try_parse_from([
            "whisper-dictate",
            "inject-text",
            "hi",
            "--backend",
            "notabackend",
        ]);
        assert!(err.is_err(), "expected clap to reject unknown backend");
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
        // Bare `devices` still parses (existing hidden JSON envelope helper
        // vp_devices.py shells out to via VOICEPI_DEVICES_BACKEND=rust).
        let cli = Cli::parse_from(["whisper-dictate", "devices"]);
        assert_eq!(cli.command, Some(Command::Devices { command: None }));
    }

    #[test]
    fn parses_devices_test_subcommand() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "devices",
            "test",
            "Microphone (Yeti Classic)",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::Devices {
                command: Some(DevicesCommand::Test {
                    name: "Microphone (Yeti Classic)".to_owned(),
                }),
            })
        );
    }

    #[test]
    fn parses_devices_test_empty_name_for_system_default() {
        // Empty string is the documented way to test the system default input.
        let cli = Cli::parse_from(["whisper-dictate", "devices", "test", ""]);
        assert_eq!(
            cli.command,
            Some(Command::Devices {
                command: Some(DevicesCommand::Test {
                    name: String::new(),
                }),
            })
        );
    }

    #[test]
    fn parses_devices_test_with_hyphen_leading_name() {
        // Rare hardware names can start with `-`; clap must let it through so
        // the Python resolver decides "not found" rather than clap rejecting
        // it as a stray flag. Matches the corpus-record hyphen precedent.
        let cli = Cli::parse_from(["whisper-dictate", "devices", "test", "-hyphen-mic"]);
        assert_eq!(
            cli.command,
            Some(Command::Devices {
                command: Some(DevicesCommand::Test {
                    name: "-hyphen-mic".to_owned(),
                }),
            })
        );
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
    fn parses_hotkey_capture_default_flags() {
        let cli = Cli::parse_from(["whisper-dictate", "hotkey", "capture"]);
        assert_eq!(
            cli.command,
            Some(Command::Hotkey {
                command: HotkeyCommand::Capture {
                    for_secs: "5".to_owned(),
                    json: false,
                    exit_on_chord: false,
                    config: None,
                },
            })
        );
    }

    #[test]
    fn parses_hotkey_capture_all_flags() {
        let cli = Cli::parse_from([
            "whisper-dictate",
            "hotkey",
            "capture",
            "--for",
            "0.5",
            "--json",
            "--exit-on-chord",
            "--config",
            "/tmp/cfg.json",
        ]);
        assert_eq!(
            cli.command,
            Some(Command::Hotkey {
                command: HotkeyCommand::Capture {
                    for_secs: "0.5".to_owned(),
                    json: true,
                    exit_on_chord: true,
                    config: Some("/tmp/cfg.json".to_owned()),
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
