//! Worker command construction and the CLI knobs that shape the child
//! Python process invocation.
//!
//! Split out of `runtime.rs` in the 500-LOC modularity refactor. Owns
//! [`WorkerCommand`] / [`PlannedCommand`] / [`Platform`] plus the full
//! set of command constructors ([`worker_command`], [`doctor_command`],
//! [`install_command`], [`audio_devices_command`], the benchmark /
//! corpus / windows / test-device variants, …) and the venv / app-root
//! path resolvers they share.

use std::env;
use std::path::{Path, PathBuf};

use crate::config;

use super::hotkey_install::normalise_hotkey_aliases_for_python;

pub(crate) const PYTHON_ENV: &str = "VOICEPI_PYTHON";
pub(crate) const APP_ROOT_ENV: &str = "VOICEPI_APP_ROOT";
pub(crate) const WORKER_EVENTS_ENV: &str = "VOICEPI_WORKER_EVENTS";
pub(crate) const RUST_INJECTOR_ENV: &str = "VOICEPI_RUST_INJECTOR";
pub(crate) const PYTHONPATH_ENV: &str = "PYTHONPATH";

/// Opt-in switch for the experimental Rust-side capture pipeline (cpal +
/// rubato + Silero via vad-rs). Read at supervisor start. Has no effect
/// unless the binary was compiled with the `audio-in-rust` cargo feature
/// AND the env var is set to a truthy value. See
/// [`audio_pipeline_requested`] for the parsing.
pub const AUDIO_BACKEND_ENV: &str = "VOICEPI_AUDIO_BACKEND";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
}

impl WorkerCommand {
    pub fn display(&self) -> String {
        let mut parts = vec![self.program.display().to_string()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    Windows,
    Unix,
}

impl Platform {
    pub(crate) fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
}

impl PlannedCommand {
    pub(crate) fn display(&self) -> String {
        let mut parts = vec![self.program.display().to_string()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

pub(crate) fn python_source_root(app_root: &Path) -> PathBuf {
    app_root.join("src").join("python")
}

pub fn worker_command(app_root: impl AsRef<Path>) -> WorkerCommand {
    worker_command_with_args(app_root, Vec::<String>::new())
}

pub fn worker_command_with_args(
    app_root: impl AsRef<Path>,
    passthrough_args: impl IntoIterator<Item = String>,
) -> WorkerCommand {
    let app_root = app_root.as_ref().to_path_buf();
    let mut args = vec![
        "-m".to_owned(),
        "whisper_dictate.runtime".to_owned(),
        "--app-root".to_owned(),
        app_root.display().to_string(),
    ];
    args.extend(passthrough_args);
    let mut env = vec![(
        PYTHONPATH_ENV.to_owned(),
        python_source_root(&app_root).display().to_string(),
    )];
    env.extend(config::worker_env_overrides());
    normalise_hotkey_aliases_for_python(&mut env);
    // Export the CLI binary path — NOT the running exe. When the tray was
    // launched from `whisper-dictate-gui.exe`, `current_exe()` points at the
    // GUI binary, which has no CLI surface (it ignores args and just opens
    // the tray). The Python worker uses this env var to shell out for
    // Rust-side helpers (cloud-transcribe, etc.); shelling out to the GUI
    // binary would silently launch a second tray window instead of running
    // the intended verb. `cli_exe_path()` resolves the sibling CLI binary
    // when we are running as the GUI, and is a no-op otherwise.
    env.push((
        RUST_INJECTOR_ENV.to_owned(),
        cli_exe_path().display().to_string(),
    ));
    // NB: the Rust-hotkey "park Python listener" flag (VOICEPI_PYTHON_HOTKEY=0)
    // used to be added here automatically based on env-var + feature gates,
    // but that's unsound — the gates only say what the user *requested*,
    // not whether the Rust listener actually came up. If we disabled
    // Python before confirming Rust was wired, a startup failure (no X
    // display, missing macOS accessibility permission, ...) left BOTH
    // backends inert and PTT permanently broken (Codex P1 on PR #344).
    // The supervisor now calls [`disable_python_hotkey`] explicitly only
    // AFTER `maybe_install_rust_hotkey` returns a live handle.
    WorkerCommand {
        program: python_program(),
        args,
        working_dir: app_root,
        env,
    }
}

pub fn default_worker_command() -> WorkerCommand {
    worker_command(app_root())
}

pub fn default_worker_command_with_args(args: Vec<String>) -> WorkerCommand {
    worker_command_with_args(app_root(), args)
}

pub fn doctor_command() -> WorkerCommand {
    default_worker_command_with_args(vec!["--doctor".to_owned()])
}

/// Whether the user requested the Rust-side audio pipeline via
/// `VOICEPI_AUDIO_BACKEND=rust`. Returns false for unset / empty / any
/// non-`rust` value. Pure helper so the gate is unit-testable without
/// spawning a worker.
pub fn audio_pipeline_requested() -> bool {
    env::var(AUDIO_BACKEND_ENV)
        .map(|value| value.trim().eq_ignore_ascii_case("rust"))
        .unwrap_or(false)
}

/// Whether the running binary can actually serve the request. The Rust
/// pipeline is gated behind the `audio-in-rust` cargo feature so a stock
/// build returns false even if the env var is set. The supervisor logs
/// a one-line warning and falls back to the Python sounddevice path in
/// that case so the user is never silently surprised.
pub fn audio_pipeline_available() -> bool {
    cfg!(feature = "audio-in-rust")
}

/// Worker command that lists input (microphone) devices as JSON and exits
/// without loading a model or opening audio. Drives the Speech tab's "Refresh
/// devices" action.
///
/// When the supervisor's parent env has `VOICEPI_AUDIO_BACKEND=rust`, the
/// command also exports `VOICEPI_DEVICES_BACKEND=rust` to the worker so
/// the Python `list_input_devices` shells out to the Rust enumeration
/// helper (`whisper-dictate devices`) instead of querying sounddevice.
/// Without this propagation the picker would still surface non-default-host
/// devices that the active Rust capture pipeline cannot open — a user that
/// opted into Rust capture via the single documented env var would be left
/// to discover the second `VOICEPI_DEVICES_BACKEND=rust` knob to keep the
/// picker honest (P3 #376, late Codex finding on PR #369).
///
/// On a binary built WITHOUT `audio-in-rust` the Rust `devices` subcommand
/// prints a structured error and exits non-zero, so the Python shell-out
/// falls back to sounddevice transparently — the propagation is therefore
/// safe to apply unconditionally and we do not gate it on the feature here.
pub fn audio_devices_command() -> WorkerCommand {
    let mut command = default_worker_command_with_args(vec!["--list-audio-devices".to_owned()]);
    if audio_pipeline_requested() {
        propagate_rust_devices_backend(&mut command);
    }
    command
}

/// Append `VOICEPI_DEVICES_BACKEND=rust` to `command.env` when neither
/// `command.env` nor the process env already mentions the variable. Split
/// out so the precedence + idempotency contract is unit-testable and so
/// any future worker command that needs to honour Rust capture's
/// device-enumeration limit can share the helper.
///
/// Precedence (highest first):
/// 1. **`command.env` already has the key** — caller pre-populated it
///    (e.g. test setup, a derived-from-config override added upstream);
///    we leave it alone and add nothing.
/// 2. **Process env has `VOICEPI_DEVICES_BACKEND` set** — the user
///    deliberately exported a value (typically `=python` to force the
///    sounddevice path for a debug session even while Rust capture is
///    active). The spawned worker inherits the process env, so adding
///    `=rust` to `command.env` would silently override that intent.
///    Skip the propagation so the user's explicit choice wins.
/// 3. **Neither set** — add `VOICEPI_DEVICES_BACKEND=rust` so the Python
///    picker shells out to the Rust enumeration.
///
/// We deliberately do not error on a `NotUnicode` process-env value: if
/// the user set the variable to bytes we can't decode, that's a problem
/// for the Python side to surface; from the propagator's point of view
/// the variable IS set so we skip, which preserves whatever bytes the
/// inherited env carries.
pub(crate) fn propagate_rust_devices_backend(command: &mut WorkerCommand) {
    if command
        .env
        .iter()
        .any(|(key, _)| key == "VOICEPI_DEVICES_BACKEND")
    {
        return;
    }
    if env::var_os("VOICEPI_DEVICES_BACKEND").is_some() {
        return;
    }
    command
        .env
        .push(("VOICEPI_DEVICES_BACKEND".to_owned(), "rust".to_owned()));
}

/// Worker command that lists visible top-level windows as JSON and exits.
/// Drives the Profiles tab's "List open windows" action.
pub fn windows_command() -> WorkerCommand {
    default_worker_command_with_args(vec!["--list-windows".to_owned()])
}

/// Worker command that dry-run opens the named microphone (resolve + try the
/// same WASAPI/DirectSound/MME open matrix as capture, recording NO audio),
/// prints a single JSON usability result and exits without loading a model.
/// Drives the Speech tab's microphone "Test" action. An empty `name` tests the
/// system default input.
pub fn test_audio_device_command(name: &str) -> WorkerCommand {
    default_worker_command_with_args(vec!["--test-audio-device".to_owned(), name.to_owned()])
}

/// Worker command that runs the golden benchmark corpus
/// (`benchmark/corpus.json`) through the configured backend and prints per-item
/// JSONL plus a final `[benchmark]` summary line, then exits. Drives the System
/// tab's "Run benchmark" action. Slow (loads the model + runs the corpus), so
/// the UI runs it as a background task. Inherits the same `--app-root` +
/// effective-config env as every other worker command, so it uses the same
/// model/device/backend settings the dictation run would.
pub fn benchmark_command() -> WorkerCommand {
    default_worker_command_with_args(vec!["--run-benchmark".to_owned()])
}

/// Worker command that records reference audio for the golden-corpus item `id`
/// from the configured microphone (reusing the same negotiated capture path as
/// dictation) and saves it to `<appdata>/benchmark/audio/<id>.wav`, printing
/// start/progress/done JSON events. Drives the System tab's "Record" action.
/// Inherits the same `--app-root` (corpus resolution) + effective-config env
/// (so the configured microphone is used) as every other worker command.
pub fn record_corpus_item_command(id: &str) -> WorkerCommand {
    // P3 #372 finding 1: pass the id as a single `--flag=value` arg rather
    // than two adjacent tokens. Python argparse processes `--flag value`
    // by greedy lookahead, and a value that starts with `-` (legal in
    // is_safe_corpus_id which allows `[A-Za-z0-9._-]`) can be parsed as
    // an unknown flag — silently dropping or mis-routing the id. The
    // `--flag=value` form is unambiguous regardless of how the value
    // starts. `is_safe_corpus_id` upstream already forbids `=`, so the
    // joined token is safe to round-trip through argparse.
    default_worker_command_with_args(vec![format!("--record-corpus-item={id}")])
}

/// The app root used to resolve bundled resources (e.g. `benchmark/corpus.json`).
/// Public wrapper over the internal resolver so the UI can read the SAME corpus
/// manifest the worker does, honoring `VOICEPI_APP_ROOT` / the installed layout.
pub fn resource_app_root() -> PathBuf {
    app_root()
}

pub fn install_command() -> WorkerCommand {
    // MUST be the CLI binary. `install_command()` invokes `<exe> install`,
    // which is a CLI verb — the GUI binary (`whisper-dictate-gui.exe`) has
    // no CLI surface, so a naive `current_exe()` here would open a second
    // tray window when the Settings UI's Install/repair button is pressed.
    // `cli_exe_path()` resolves the sibling CLI binary in that case.
    install_command_from_exe(cli_exe_path(), app_root())
}

/// Path to the CLI binary (`whisper-dictate[.exe]`) — the one every internal
/// spawn (worker `VOICEPI_RUST_INJECTOR`, Settings UI "Install / repair", the
/// Python-to-Rust helper shell-outs) actually wants.
///
/// When the running process IS the CLI binary — the normal case for every
/// spawn path (the CLI shells out to the worker; the tray sends the worker
/// events but doesn't spawn CLI verbs from within itself very often), this is
/// just `current_exe()`.
///
/// When the running process is the sibling GUI binary
/// (`whisper-dictate-gui[.exe]`) — the tray/settings UI entry from the
/// windows-subsystem split — this looks up the CLI binary next to it in the
/// same install directory. Both the Inno installer and the portable ZIP ship
/// the two binaries side by side so the lookup is stable in shipping layouts.
///
/// Falls back to `current_exe()` unchanged when the running exe name is
/// unexpected (e.g. a renamed dev build). The failure mode there is a clear
/// "unknown CLI verb" error from the launched binary — not a silent tray
/// re-launch, which is what makes this bug hard to spot.
pub fn cli_exe_path() -> PathBuf {
    let current = env::current_exe().unwrap_or_else(|_| PathBuf::from("whisper-dictate"));
    cli_exe_from(&current)
}

/// Pure variant of [`cli_exe_path`] — takes the running exe path so unit
/// tests can exercise the resolution rule on any platform without spawning.
pub(crate) fn cli_exe_from(current: &Path) -> PathBuf {
    let Some(file_name) = current.file_name().and_then(|f| f.to_str()) else {
        return current.to_path_buf();
    };
    let lower = file_name.to_ascii_lowercase();
    let (stem, has_exe) = match lower.strip_suffix(".exe") {
        Some(rest) => (rest, true),
        None => (lower.as_str(), false),
    };
    if stem != "whisper-dictate-gui" {
        return current.to_path_buf();
    }
    let sibling = if has_exe {
        "whisper-dictate.exe"
    } else {
        "whisper-dictate"
    };
    current.with_file_name(sibling)
}

pub fn install_command_from_exe(
    exe: impl Into<PathBuf>,
    app_root: impl Into<PathBuf>,
) -> WorkerCommand {
    WorkerCommand {
        program: exe.into(),
        args: vec!["install".to_owned()],
        working_dir: app_root.into(),
        env: Vec::new(),
    }
}

pub(crate) fn python_program() -> PathBuf {
    if let Some(raw) = env::var_os(PYTHON_ENV) {
        return PathBuf::from(raw);
    }
    if let Some(path) = default_venv_python() {
        return path;
    }
    PathBuf::from(default_python_name())
}

fn default_venv_python() -> Option<PathBuf> {
    let home = home_dir()?;
    let path = venv_python_path(
        &default_venv_dir(&home, Platform::current()),
        Platform::current(),
    );
    path.exists().then_some(path)
}

pub(crate) fn default_venv_dir(home: &Path, platform: Platform) -> PathBuf {
    match platform {
        Platform::Windows => windows_venv_dir(home),
        Platform::Unix => home.join(".venv-whisper-dictate"),
    }
}

/// Resolve the Windows venv directory with legacy-fallback logic.
///
/// Resolution order (no forced migration — existing installs keep working):
/// 1. `<home>\whisper-dictate-venv` — the canonical post-rebrand name.
/// 2. `<home>\voice-pi-venv`        — legacy pre-rebrand name; kept as-is.
/// 3. `<home>\whisper-dictate-venv` — fresh-install default (neither exists).
pub(crate) fn windows_venv_dir(home: &Path) -> PathBuf {
    // is_dir(), not exists(): a stray FILE at either path must not be selected
    // as the venv (python -m venv would then fail on the existing file).
    let new_name = home.join("whisper-dictate-venv");
    if new_name.is_dir() {
        return new_name;
    }
    let legacy = home.join("voice-pi-venv");
    if legacy.is_dir() {
        return legacy;
    }
    new_name
}

pub(crate) fn venv_python_path(venv_dir: &Path, platform: Platform) -> PathBuf {
    match platform {
        Platform::Windows => venv_dir.join("Scripts").join("python.exe"),
        Platform::Unix => venv_dir.join("bin").join("python"),
    }
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub(crate) fn default_python_name() -> &'static str {
    if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    }
}

/// Source-checkout fallback for [`app_root`] when the running exe is not in
/// an installed layout (typical case: `cargo run` / `cargo build && ./target/release/...`).
///
/// `CARGO_MANIFEST_DIR` points at this crate — `<repo>/src/rust`. The repo
/// root is two levels up (`ancestors().nth(2)`): index 0 = `src/rust`,
/// index 1 = `src`, index 2 = repo root. The previous `.nth(3)` walked one
/// level too far and produced the parent of the repo (e.g. `D:\source`
/// rather than `D:\source\whisper-dictate`), which made every worker spawn
/// from `target/release/` fail with `ModuleNotFoundError` because the
/// resulting PYTHONPATH (`<parent>/src/python`) did not exist. Installed
/// builds are unaffected because `app_root_from_exe_path` succeeds there.
pub(crate) fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

pub(crate) fn app_root() -> PathBuf {
    if let Some(raw) = env::var_os(APP_ROOT_ENV) {
        return PathBuf::from(raw);
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(root) = app_root_from_exe_path(&exe) {
            return root;
        }
    }
    source_root()
}

pub(crate) fn app_root_from_exe_path(exe: &Path) -> Option<PathBuf> {
    let root = exe.parent()?;
    python_source_root(root)
        .join("whisper_dictate")
        .join("runtime.py")
        .exists()
        .then(|| root.to_path_buf())
}
