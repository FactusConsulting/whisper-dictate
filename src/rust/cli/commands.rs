use super::{
    ConfigCommand, DevicesCommand, DictionaryCommand, HistoryCommand, HotkeyCommand, ModelsCommand,
    SelfTestCommand,
};
use clap::Subcommand;

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum Command {
    /// Open the desktop UI.
    Ui,
    /// Open settings in the desktop UI.
    Settings,
    /// Run dictation in the terminal.
    Run {
        /// Per-run native dictation overrides.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List visible top-level Windows windows as JSON.
    ListWindows,
    /// Transcribe a 16 kHz mono WAV file with the configured Rust STT backend.
    ///
    /// Uses local whisper.cpp when `stt_backend=whisper`, or the configured
    /// OpenAI-compatible cloud endpoint (including Groq) when
    /// `stt_backend=openai`. This command never falls back to Python.
    TranscribeFile {
        /// Input WAV file. Convert other formats with:
        /// `ffmpeg -i INPUT -ac 1 -ar 16000 OUTPUT.wav`.
        #[arg(value_name = "PATH")]
        path: String,
        /// Emit one machine-readable JSON object instead of transcript text.
        #[arg(long)]
        json: bool,
    },
    /// Record a bounded microphone sample and recommend audio thresholds.
    CalibrateMic {
        /// Recording duration in seconds.
        #[arg(default_value_t = 5.0)]
        seconds: f64,
        /// Input device name. Defaults to the configured microphone.
        #[arg(long)]
        device: Option<String>,
        /// Emit one machine-readable JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Analyze a 16 kHz mono WAV file and recommend audio thresholds.
    CalibrateFile {
        /// Input WAV file.
        #[arg(value_name = "PATH")]
        path: String,
        /// Emit one machine-readable JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Report platform readiness for dictation: OS + session, Whisper models
    /// cache, injection helper availability, audio input,
    /// config file validity, and whether the configured model is downloaded.
    ///
    /// Exit code is 0 when no check `fail`s (warnings are non-blocking) and
    /// 1 otherwise, so the same command drives interactive troubleshooting
    /// and CI smoke gating. Every check is READ-ONLY - the doctor never
    /// writes to config, downloads models, or opens the display server.
    /// Audit item 2 chunk E.
    Doctor {
        /// Emit machine-readable JSON:
        /// `{"checks":[{"name","status","detail"},...],"summary":{"ok","warn","fail"}}`.
        #[arg(long)]
        json: bool,
        /// Override the config file path used by the `config` /
        /// `configured-model` checks. Same precedence rule as `config get`:
        /// this flag > `VOICEPI_CONFIG` env var > platform user config.
        #[arg(long, value_name = "PATH")]
        config: Option<String>,
    },
    /// Run the golden benchmark corpus through the configured backend and
    /// print a one-line `[benchmark] ...` summary. Same code path as the
    /// "Run benchmark" button - the corpus is resolved relative to the app
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
        /// out - defence in depth on top of the worker's own guard.
        #[arg(allow_hyphen_values = true)]
        id: String,
    },
    /// Drive the in-process Rust `DictateSession` over a WAV file, offline,
    /// via the cloud STT backend (Groq/OpenAI). Rust-native, WAV-driven
    /// counterpart of `dictate-mic` (which reads a live microphone).
    /// Injection is preview-only. Used to CLI-integration-test the Rust
    /// engine; hidden because it needs a cloud key and is primarily a
    /// CI/diagnostic tool. Superseded the retired `simulate-ptt` verb (which
    /// forwarded to a Python-side pipeline).
    #[command(hide = true)]
    SimulateSession {
        /// WAV file (16 kHz mono) to feed through the session.
        #[arg(long)]
        wav: String,
        /// Stream the session's worker events as JSON (one object per line)
        /// instead of printing the final transcript.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Drive this many consecutive press/release cycles through the
        /// SAME session (default 1). Exercises session reuse across
        /// utterances -- the regression guard for the "PTT only works the
        /// first time, then gets stuck" class of bug. A value of 0 is
        /// treated as 1.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
    /// Capture live microphone audio through the Rust audio pipeline and drive
    /// the in-process Rust `DictateSession` over it: the fully-Rust,
    /// no-Python live-capture counterpart of `simulate-session` (which reads a
    /// WAV). Records for `--seconds`, resamples via the VAD-free capture pump
    /// (`audio::raw`), transcribes over the cloud STT backend (Groq/OpenAI),
    /// and previews the injected text (nothing is typed into the OS).
    ///
    /// Feature-gated behind `audio-capture` (cpal + rubato). On a stock build
    /// the CLI surface still parses but the handler exits non-zero with an
    /// actionable "rebuild with --features audio-capture" message. Hidden
    /// because it needs a cloud key + a real mic and is primarily a diagnostic
    /// tool for the Rust capture path.
    #[command(hide = true)]
    DictateMic {
        /// Input device name (exact or longest-substring match, same
        /// precedence as `devices`). Empty selects the host default input.
        #[arg(long, default_value = "")]
        device: String,
        /// How long to record before transcribing, in seconds.
        #[arg(long, default_value_t = 3.0)]
        seconds: f64,
        /// Stream the session's worker events as JSON (one object per line)
        /// instead of printing the final transcript.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Run the Ubuntu Wayland desktop setup helper.
    SetupUbuntu,
    /// Interactively configure WD.
    Setup,
    /// Export effective config and shell environment lines.
    ExportConfig {
        /// Include API-key values. By default every discovered secret is redacted.
        #[arg(long)]
        include_secrets: bool,
    },
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
    /// Manage the custom dictionary.
    Dictionary {
        #[command(subcommand)]
        command: DictionaryCommand,
    },
    /// Internal JSON helper for dictionary prompt and replacement operations.
    #[command(hide = true)]
    DictionaryRuntime,
    /// Internal JSON helper for dictation pure-logic decisions. Reads an
    /// envelope on stdin
    /// (`{"op": "...", "params": {...}}`) and writes a JSON response on
    /// stdout. See `src/rust/dictate/ops.rs` for the op catalogue.
    #[command(hide = true)]
    DictateOps,
    /// Inspect local dictation history.
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
    /// Foreground driver for the full Rust dictation runtime: installs the
    /// hotkey listener + coordinator, wires them into the in-process
    /// `DictateSession` action sink (real backends when the required cargo
    /// features are compiled in - see below), and runs until Ctrl-C.
    ///
    /// Feature-gated behind `rust-hotkeys,rust-injection`. On a stock build
    /// the CLI surface still parses (so smoke scripts can pin it without a
    /// shell-level feature detect) but the handler exits non-zero with an
    /// actionable "rebuild with --features ..." message. Matching sibling
    /// features (`whisper-rs-local` for real Whisper inference,
    /// `audio-capture` for the cpal capture pump) upgrade the session from
    /// PR 4 stubs to the real backends via
    /// `runtime::rust_session_sink::build_production_sink`.
    DictateRun {
        /// Override the config file path (default: platform user config;
        /// also honours `VOICEPI_CONFIG` when this flag is omitted). Same
        /// precedence rule as `config get --config` / `doctor --config`.
        #[arg(long, value_name = "PATH")]
        config: Option<String>,
        /// Emit machine-readable JSONL on stdout instead of the human-
        /// readable `[dictate-run] ...` lines. First line is a stable
        /// `{"kind":"ready","ready":true,"engine":"rust","chord":"...","driver":"..."}`
        /// envelope so a supervising parent process can gate on it before
        /// forwarding OS key events; subsequent lines are one JSON per
        /// `RuntimeEvent` observed. The `ready` envelope's keys are the
        /// machine-readable contract callers should pin.
        #[arg(long, default_value_t = false)]
        json_events: bool,
        /// Documentation flag - the verb always runs in the foreground
        /// today (the process itself IS the dictation runtime). Reserved for
        /// a possible future background variant; currently a no-op.
        #[arg(long, default_value_t = false)]
        foreground: bool,
    },
    /// Headless self-tests for the shipped runtime - regression checks that
    /// exercise past-breakage classes without needing a display server, audio
    /// hardware, or root. Every sub-verb takes zero configuration and exits
    /// non-zero on any observed regression, so CI (and the wayland-user-smoke
    /// script) can pin them without a shell-level feature detect.
    SelfTest {
        #[command(subcommand)]
        command: SelfTestCommand,
    },
    /// Inject text into the active window - scripting + smoke-test wrapper
    /// around the injection library. **Defaults to `--dry-run`**: the
    /// resolved backend + keystroke plan is printed and NOTHING is typed.
    /// Real injection requires `--do-it` (alias `--live`).
    ///
    /// Two invocation shapes coexist:
    ///
    /// * `inject-text <TEXT> [--dry-run|--do-it] [--backend NAME] [--json]`
    ///   - the public scripting/smoke form (audit item 2 chunk B).
    /// * `inject-text --mode {type|paste} --text ... --xkb-layout ...
    ///   --target-title ... --target-process ...` - the internal Wayland
    ///   layout-aware typing form. The public path never sets `--mode`.
    InjectText {
        /// Text to inject (public form - positional). When present the
        /// command runs the dry-run/inject flow; when absent the legacy
        /// `--mode` + `--text` helper path runs.
        #[arg(value_name = "TEXT")]
        text_arg: Option<String>,
        /// Explicit dry-run flag (matches the default). Set for clarity /
        /// self-documenting shell scripts; `--do-it` overrides.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// REALLY inject the text into the active window (dangerous -
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
        /// human-readable summary - the JSON keys are stable so tests can
        /// pin them.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Internal mode selector (`type` / `paste`). A non-empty value
        /// selects the Wayland keycode path; leave empty for the public
        /// dry-run form.
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
    /// Internal helper for post-STT formatting.
    #[command(hide = true)]
    FormatText {
        /// Text to format.
        #[arg(long)]
        text: String,
        /// Spoken formatting command set: off, en, da, or both.
        #[arg(long, default_value = "off")]
        command_set: String,
    },
    /// Internal helper for cloud STT.
    #[command(hide = true)]
    CloudTranscribe {
        /// OpenAI-compatible API base URL.
        #[arg(long)]
        base_url: String,
        /// API key. OPTIONAL and discouraged: a process command line is
        /// readable by other local users (`ps aux`, `/proc/<pid>/cmdline`),
        /// so passing a secret here leaks it for the lifetime of the call.
        /// Prefer the environment: `VOICEPI_STT_API_KEY`, or `GROQ_API_KEY`
        /// / `OPENAI_API_KEY` matching `--base-url`. Kept for backwards
        /// compatibility with older callers.
        #[arg(long, default_value = "")]
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
    /// Internal helper to append JSONL safely.
    #[command(hide = true)]
    AppendJsonl {
        /// JSONL file to append to.
        #[arg(long)]
        path: String,
    },
    /// Internal helper to append filtered history.
    #[command(hide = true)]
    AppendHistory {
        /// History JSONL file to append to.
        #[arg(long)]
        path: String,
    },
    /// Internal helper to append metrics + history.
    #[command(hide = true)]
    AppendRecordSinks,
    /// Internal helper to emit controller events.
    #[command(hide = true)]
    WorkerEvent,
    /// Internal helper to run command hooks.
    #[command(hide = true)]
    CommandHook,
    /// Internal helper for cloud-safe redaction.
    #[command(hide = true)]
    RedactText,
    /// Internal helper for target profile matching.
    #[command(hide = true)]
    ApplyProfile,
    /// Internal helper for local-only checks.
    #[command(hide = true)]
    Privacy,
    /// Internal helper for post-STT formatting / LLM cleanup. JSON envelope
    /// on stdin, JSON response on stdout - see `src/rust/postprocess.rs`.
    #[command(hide = true)]
    Postprocess,
    /// Internal helper for OpenAI-compatible chat completion (post-processor
    /// cloud backend) and transcription prompt capping. JSON envelope on
    /// stdin, JSON response on stdout - see `src/rust/cloud_api/chat.rs`.
    #[command(hide = true)]
    ExternalApi,
    /// Internal helper: render the `[health]` line or compute the 4-level grade.
    #[command(hide = true)]
    Health,
    /// Internal one-shot local Whisper JSON dispatch. Only does real work
    /// when the binary was built with `--features whisper-rs-local`;
    /// otherwise exits non-zero with a clear "feature not compiled in"
    /// message. See `src/rust/whisper/dispatch.rs`.
    ///
    /// The `--probe` flag short-circuits before reading stdin / the model env
    /// var: it exits 0 on a feature-enabled build and non-zero on a stock
    /// build, allowing callers to check native inference support without
    /// loading a model.
    #[command(hide = true)]
    TranscribeWav {
        /// Probe-only mode: do not read stdin or run inference; exit 0 iff
        /// the binary was built with `--features whisper-rs-local`.
        #[arg(long)]
        probe: bool,
    },
    /// Long-running in-process Whisper worker. Reads one JSON request per
    /// `\n`-terminated line from stdin, writes one JSON response per line to
    /// stdout - same envelope as `transcribe-wav` but the GGML model is
    /// loaded ONCE and stays resident between calls (subject to
    /// `VOICEPI_WHISPER_IDLE_UNLOAD_S`). Per-request errors stay in-protocol
    /// as `{"error":"..."}` envelopes so a single bad request does not tear
    /// down the worker. See `src/rust/whisper/dispatch.rs::handle_transcribe_server`.
    ///
    /// First stdout line is a `ServerReady` envelope so callers can confirm
    /// the binary supports long-running mode before sending requests.
    #[command(hide = true)]
    TranscribeServer,
    /// Internal cross-platform injection dispatch: reads a JSON request
    /// envelope on stdin and writes a JSON response on stdout.
    #[command(hide = true)]
    Inject,
    /// Enumerate input audio devices as JSON, or dry-run test a specific
    /// microphone. With no subcommand the binary reads a JSON envelope on
    /// stdin (`{"action":"list"|"default"|"find","query":"..."}`) and prints
    /// the matching JSON response. Built into binaries with the
    /// `audio-capture` feature; binaries without the feature print a
    /// structured error and exit non-zero.
    ///
    /// The `test <NAME>` subcommand runs the native cpal probe in
    /// `src/rust/audio/device_probe.rs` (which reuses the same live-capture
    /// open matrix), prints a single JSON usability result to stdout and
    /// exits - no ML model is loaded. Pass an empty string to test the
    /// system default input. Requires the `audio-capture` feature (the
    /// shipping binary always ships with it); non-`audio-capture` dev
    /// builds emit a "rebuild with --features audio-capture" refusal and
    /// exit non-zero. Enables headless mic verification in the CI container
    /// See `docs/ARCHITECTURE.md` for the native device-probe boundary.
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
