use std::env;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(feature = "audio-in-rust")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
#[cfg(feature = "audio-in-rust")]
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::config;

const APP_ROOT_ENV: &str = "VOICEPI_APP_ROOT";
const WORKER_EVENTS_ENV: &str = "VOICEPI_WORKER_EVENTS";
const WORKER_EVENT_PREFIX: &str = "[worker-event] ";
/// Opt-in switch for the experimental Rust-side capture pipeline (cpal +
/// rubato + Silero via vad-rs). Read at supervisor start. Has no effect
/// unless the binary was compiled with the `audio-in-rust` cargo feature
/// AND the env var is set to a truthy value. See
/// [`audio_pipeline_requested`] for the parsing.
pub const AUDIO_BACKEND_ENV: &str = "VOICEPI_AUDIO_BACKEND";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Stopped,
    Starting,
    Running,
}

impl RuntimeState {
    pub fn label(self) -> &'static str {
        match self {
            RuntimeState::Stopped => "Stopped",
            RuntimeState::Starting => "Starting",
            RuntimeState::Running => "Running",
        }
    }
}

/// `whisper-dictate run` — foreground dictation.
///
/// Wave 8 Part 2: v1.20 dropped the Python worker; the dictation
/// runtime is now the in-process `worker-rust` subcommand. Rather than
/// spawning a subprocess (which was necessary when the worker was a
/// separate `python -m whisper_dictate.runtime` process), foreground
/// `run` invokes [`worker_rust::handle_worker_rust`] directly.
///
/// The delegate gate ([`worker_rust::should_delegate_to_worker_rust`])
/// is consulted first with the effective worker env; a declined gate
/// (missing feature build / unsupported settings) surfaces the reason
/// via a hard error instead of silently doing nothing, matching the
/// no-fallback contract of the supervisor path.
///
/// Passthrough `args` from `whisper-dictate run --foo bar` are silently
/// ignored — the Rust worker reads all of its configuration from env
/// vars (materialised by [`config::worker_env_overrides`]). A user
/// that passes CLI flags here is likely reaching for the pre-Wave-8
/// Python `argparse` surface; the warning documents the change.
pub fn run_terminal(args: Vec<String>) -> Result<()> {
    if !args.is_empty() {
        eprintln!(
            "warning: `whisper-dictate run` no longer forwards CLI flags to the worker; \
             the Rust dictation runtime reads its settings from VOICEPI_* env vars. \
             Ignored args: {args:?}"
        );
    }
    let env_overrides = config::worker_env_overrides();
    // Codex #453 P2: nudge users with a stale python-legacy escape
    // hatch to unset it now that Python is gone.
    worker_rust::warn_stale_python_legacy_if_set(&env_overrides);
    if !worker_rust::should_delegate_to_worker_rust(&env_overrides) {
        let reason = worker_rust::unsupported_worker_rust_settings_reason(&env_overrides)
            .unwrap_or_else(|| {
                "this build was compiled without the full rust-session feature set \
                 (whisper-rs-local + rust-injection + audio-in-rust + rust-hotkeys); \
                 rebuild with those features to enable dictation"
                    .to_owned()
            });
        return Err(anyhow!(
            "cannot run dictation: {reason}. Python worker was removed in v1.20."
        ));
    }
    // Codex #453 P1: the in-process worker reads its configuration
    // from `std::env::vars()` (via `WorkerRunner::from_env`); the
    // supervisor path plumbs `env_overrides` through the child's
    // `Command::envs`, but this foreground path spawns nothing --
    // it invokes the worker in-process. Without this apply loop the
    // defaults-from-schema (VOICEPI_KEY, VOICEPI_STT_BACKEND, model
    // paths, etc.) are missing and the worker fails to install the
    // hotkey listener / build the backend. Applying the overrides
    // to `std::env` is safe because this process is dedicated to the
    // worker for its entire remaining lifetime.
    for (key, value) in &env_overrides {
        // SAFETY: single-threaded pre-worker init; no other thread
        // observes std::env here. `worker_env_overrides()` never
        // returns keys that are `std::env`-reserved (empty / with `=`
        // in the key or a NUL byte).
        std::env::set_var(key, value);
    }
    worker_rust::handle_worker_rust(false)
}

/// `whisper-dictate doctor` — dependency + platform readiness check.
///
/// Wave 8 Part 2: the pre-v1.20 doctor spawned the Python worker with
/// `--doctor` so `vp_doctor.py` could probe `sounddevice`,
/// `faster-whisper`, `pynput`, X display availability, etc. That
/// module was deleted alongside the rest of the Python bundle.
///
/// A native-Rust doctor that reports on cpal / whisper.cpp / rdev /
/// enigo will land in a follow-up; for v1.20 rc.1 the command reports
/// what changed and points the user at the surviving diagnostics
/// (`whisper-dictate --version`, `whisper-dictate models list`).
pub fn doctor() -> Result<()> {
    println!("whisper-dictate {}", version());
    println!(
        "The Python-based `doctor` diagnostics were removed in v1.20 along with the Python \
         worker. A native-Rust doctor is tracked as a follow-up to #348."
    );
    println!("Meanwhile:");
    println!("  * confirm your build has the rust-session features:  `whisper-dictate worker-rust --stdin-only` prints a clear error on stock builds.");
    println!(
        "  * inspect the model cache:                            `whisper-dictate models list`"
    );
    println!(
        "  * inspect the effective config:                       `whisper-dictate config show`"
    );
    Ok(())
}

pub fn install() -> Result<()> {
    // Wave 8 Part 2: the historic `install` step created a Python virtual
    // environment under `~/.venv-whisper-dictate` (Unix) or
    // `~/whisper-dictate-venv` (Windows) and ran `pip install -r
    // requirements/{cpu,gpu}.txt` inside it. v1.20 dropped the entire
    // Python bundle in favour of the in-process Rust worker, so there is
    // no longer anything to install: the packaged binary carries every
    // native dependency it needs.
    //
    // Kept as a subcommand (rather than dropped from the CLI surface) so
    // the packaging scripts, docs and legacy install scripts that still
    // shell out to `whisper-dictate install` don't error out on an
    // unknown command; the invocation is now a graceful no-op that
    // documents where the runtime deps went.
    println!(
        "whisper-dictate v1.20 no longer requires a Python venv or pip install. \
         Everything ships in the native binary. If you want to download a Whisper \
         model into the cache, run `whisper-dictate models download tiny.en` \
         (or another catalog entry)."
    );
    Ok(())
}

pub fn setup_ubuntu() -> Result<()> {
    let root = app_root();
    let script = ubuntu_setup_script_path(&root);
    if !script.exists() {
        return Err(anyhow!(
            "Ubuntu setup script not found at {}",
            script.display()
        ));
    }
    let status = Command::new("bash")
        .arg(&script)
        .env("VOICEPI_RUST_OWNS_DESKTOP", "1")
        .status()?;
    if status.success() {
        install_linux_desktop_entries()?;
        start_linux_ui_detached()?;
        Ok(())
    } else {
        Err(anyhow!(
            "Ubuntu setup failed with exit code {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

fn ubuntu_setup_script_path(root: &Path) -> PathBuf {
    let packaging_path = root
        .join("packaging")
        .join("linux")
        .join("ubuntu26.04")
        .join("setup.sh");
    if packaging_path.exists() {
        return packaging_path;
    }
    root.join("ubuntu26.04").join("setup.sh")
}

fn install_linux_desktop_entries() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    let home = home_dir().ok_or_else(|| anyhow!("HOME is not set"))?;
    let applications = home.join(".local/share/applications");
    let autostart = home.join(".config/autostart");
    std::fs::create_dir_all(&applications)?;
    std::fs::create_dir_all(&autostart)?;

    let icon = linux_app_icon_path(&home);
    let exec = linux_desktop_exec_command();
    let desktop = linux_desktop_entry(false, &exec, &icon);
    let autostart_desktop = linux_desktop_entry(true, &exec, &icon);
    let app_path = applications.join("whisper-dictate.desktop");
    let autostart_path = autostart.join("whisper-dictate.desktop");
    std::fs::write(&app_path, desktop)?;
    std::fs::write(&autostart_path, autostart_desktop)?;
    install_linux_app_icon(&home)?;

    let _ = Command::new("update-desktop-database")
        .arg(&applications)
        .status();
    println!("Desktop launcher: {}", app_path.display());
    println!("Autostart entry: {}", autostart_path.display());
    Ok(())
}

fn install_linux_app_icon(home: &Path) -> Result<()> {
    let icon_dir = linux_app_icon_path(home)
        .parent()
        .ok_or_else(|| anyhow!("invalid Linux app icon path"))?
        .to_path_buf();
    std::fs::create_dir_all(&icon_dir)?;
    std::fs::write(
        linux_app_icon_path(home),
        include_str!("../../assets/whisper-dictate-logo.svg"),
    )?;
    Ok(())
}

fn linux_app_icon_path(home: &Path) -> PathBuf {
    home.join(".local/share/icons/hicolor/scalable/apps/whisper-dictate.svg")
}

fn linux_desktop_exec_command() -> String {
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("whisper-dictate"));
    format!("{} ui", desktop_exec_token(&exe))
}

fn desktop_exec_token(path: &Path) -> String {
    let raw = path.display().to_string();
    if raw
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\\'))
    {
        format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        raw
    }
}

fn linux_desktop_entry(autostart: bool, exec: &str, icon: &Path) -> String {
    let icon = icon.display();
    let mut entry = format!(
        "[Desktop Entry]\n\
Name=Whisper Dictate\n\
Comment=Push-to-talk dictation settings and runtime control\n\
Exec={exec}\n\
Icon={icon}\n\
Terminal=false\n\
Type=Application\n\
Categories=Utility;AudioVideo;Audio;\n\
StartupNotify=true\n",
    );
    entry.push_str("StartupWMClass=whisper-dictate\n");
    if autostart {
        entry.push_str("X-GNOME-Autostart-enabled=true\n");
    }
    entry
}

fn start_linux_ui_detached() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("whisper-dictate"));
    if command_exists("gtk-launch") {
        Command::new("gtk-launch").arg("whisper-dictate").spawn()?;
        println!("Started Whisper Dictate UI via app launcher.");
    } else if command_exists("setsid") {
        Command::new("setsid").arg(&exe).arg("ui").spawn()?;
        println!("Started Whisper Dictate UI.");
    } else {
        Command::new(&exe).arg("ui").spawn()?;
        println!("Started Whisper Dictate UI.");
    }
    Ok(())
}

fn command_exists(program: &str) -> bool {
    env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|dir| dir.join(program).exists()))
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn cleanup_stale_desktop_processes() {
    if let Err(err) = cleanup_stale_desktop_processes_windows() {
        eprintln!("warning: could not clean stale whisper-dictate processes: {err}");
    }
}

#[cfg(not(windows))]
pub fn cleanup_stale_desktop_processes() {}

#[cfg(windows)]
fn cleanup_stale_desktop_processes_windows() -> Result<()> {
    let current_pid = std::process::id();
    let exe = env::current_exe()
        .ok()
        .unwrap_or_else(|| PathBuf::from("whisper-dictate.exe"));
    let app_root = app_root();
    let script = stale_process_cleanup_script(current_pid, &exe, &app_root);

    let mut command = Command::new(windows_shell_program());
    command.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        &script,
    ]);
    configure_background_process(&mut command);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("stale process cleanup exited with {status}"))
    }
}

#[cfg(windows)]
fn stale_process_cleanup_script(current_pid: u32, exe: &Path, app_root: &Path) -> String {
    let exe = escape_powershell_single_quoted(&exe.display().to_string());
    let app_root = escape_powershell_single_quoted(&app_root.display().to_string());
    format!(
        r#"
$ErrorActionPreference = 'SilentlyContinue'
$currentPid = {current_pid}
$cleanupPid = $PID
$exe = '{exe}'
$root = '{app_root}'
Get-CimInstance Win32_Process |
  Where-Object {{
    $_.ProcessId -ne $currentPid -and $_.ProcessId -ne $cleanupPid -and (
      ($_.ExecutablePath -eq $exe) -or
      ($_.CommandLine -like "*whisper_dictate.runtime*" -and $_.CommandLine -like "*$root*")
    )
  }} |
  ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}
"#
    )
}

#[cfg(windows)]
fn escape_powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(windows)]
fn windows_shell_program() -> &'static str {
    if env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|dir| dir.join("pwsh.exe").exists()))
        .unwrap_or(false)
    {
        "pwsh.exe"
    } else {
        "powershell.exe"
    }
}

pub fn version() -> String {
    let root = app_root();
    if let Ok(raw) = std::fs::read_to_string(root.join("VERSION")) {
        let version = raw.trim().trim_start_matches('v');
        if !version.is_empty() {
            return version.to_owned();
        }
    }

    if let Ok(output) = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .current_dir(&root)
        .output()
    {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout);
            let version = version.trim().trim_start_matches('v');
            if !version.is_empty() {
                return version.to_owned();
            }
        }
    }

    env!("CARGO_PKG_VERSION").to_owned()
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeEvent {
    Started { command: String },
    Worker(WorkerEvent),
    Stdout(String),
    Stderr(String),
    Exited { code: Option<i32> },
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerEvent {
    pub event: String,
    pub state: Option<String>,
    pub payload: Value,
}

/// Optional zero-arg callback fired AFTER every event is pushed onto the
/// runtime channel. Lets the consumer (the egui UI) wake itself up so an
/// event that arrives while the window is minimized / unfocused gets
/// processed immediately instead of waiting for the next 80 ms repaint
/// tick — which, on Windows, doesn't fire when the window doesn't have
/// foreground attention. Without this, the user observed the tray icon
/// staying GREEN for a full PTT cycle after ~10 min of idle: the worker
/// emitted opening/recording/transcribing as usual, but the UI never woke
/// to process the events, so the tray-state transitions never made it to
/// the OS tray API.
pub type RepaintNotifier = std::sync::Arc<dyn Fn() + Send + Sync>;

// No `#[derive(Debug)]`: `Arc<dyn Fn() + Send + Sync>` (the repaint notifier)
// does not implement Debug. Nothing in the codebase actually formats the
// supervisor with `{:?}`, so the manual impl is dead weight; leave it off
// and `#[derive(Debug)]` can return if/when a real consumer wants it.
pub struct RuntimeSupervisor {
    child: Option<Child>,
    state: RuntimeState,
    tx: Sender<RuntimeEvent>,
    rx: Receiver<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
    /// Active Rust→Python audio bridge, only `Some` when the worker was
    /// spawned with the Rust capture backend AND the worker has emitted
    /// `state=ready` (so the Python stdin reader is up). Wrapped in
    /// `Arc<Mutex<...>>` because a background "ready-watch" thread
    /// installs the handle here on receipt of the worker's ready event
    /// (iteration-3 review finding #1) — see `spawn_ready_watch`.
    /// `None` for the default Python sounddevice path AND for stock
    /// builds without the `audio-in-rust` cargo feature.
    #[cfg(feature = "audio-in-rust")]
    audio_bridge: Arc<Mutex<Option<crate::audio::BridgeHandle>>>,
    /// Set by the bridge-error watcher thread when it observes a
    /// terminal `BridgeError::Io` / `BridgeError::Pipeline` event
    /// (iteration-2 review finding #3). The watcher cannot mutate
    /// `self`, so it raises this flag; the next `poll()` call sees it
    /// and tears the worker down (kills the child, drops the bridge,
    /// emits `Exited { code: None }`) so the UI flips back to Stopped
    /// instead of leaving a half-dead worker that won't transcribe.
    #[cfg(feature = "audio-in-rust")]
    bridge_terminal: Arc<AtomicBool>,
    /// Iteration-3 review finding #1 race-guard: set by `stop()` /
    /// `poll()` teardown so the in-flight ready-watch thread (which
    /// may be mid-`pending.start()`) can detect that the supervisor
    /// no longer wants the bridge and discard the newly-opened handle
    /// instead of installing it into a stopped supervisor. Reset by
    /// `start()` on the next run.
    #[cfg(feature = "audio-in-rust")]
    bridge_cancel: Arc<AtomicBool>,
    // Wave 8 Part 2: the pre-v1.20 supervisor could optionally install a
    // Rust rdev hotkey listener directly (when
    // `VOICEPI_HOTKEY_BACKEND=rust` was set) and hold a `HotkeyHandle`
    // to drive the Python worker's dictation lifecycle from an
    // in-process CoordinatorAction sink. v1.20 makes worker-rust
    // delegation mandatory; the worker subprocess owns the rdev
    // listener and the `DictateSession` in-process, so the supervisor
    // no longer holds a hotkey handle. External-toggle CLI flags /
    // Unix signals now surface as stderr trace lines
    // (see [`Self::dispatch_external_command`]) until a
    // parent<->child IPC lands (issue #326 follow-up).
}

impl Default for RuntimeSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeSupervisor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            child: None,
            state: RuntimeState::Stopped,
            tx,
            rx,
            repaint_notifier: None,
            #[cfg(feature = "audio-in-rust")]
            audio_bridge: Arc::new(Mutex::new(None)),
            #[cfg(feature = "audio-in-rust")]
            bridge_terminal: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "audio-in-rust")]
            bridge_cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Install a callback that fires after every runtime event is enqueued.
    /// The UI installs this on its first `update()` call so the egui context
    /// is woken whenever a worker event arrives, even when the window has no
    /// foreground attention. Idempotent — overwrites any previous notifier.
    pub fn set_repaint_notifier(&mut self, notifier: RepaintNotifier) {
        self.repaint_notifier = Some(notifier);
    }

    pub fn has_repaint_notifier(&self) -> bool {
        self.repaint_notifier.is_some()
    }

    pub fn state(&self) -> RuntimeState {
        self.state
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    pub fn start(&mut self, command: WorkerCommand) -> Result<()> {
        self.poll();
        if self.child.is_some() {
            return Err(anyhow!("runtime is already running"));
        }
        // Reset the iteration-3 ready-watch cancel flag from any
        // previous run so a fresh start can install the freshly-built
        // bridge.
        #[cfg(feature = "audio-in-rust")]
        self.bridge_cancel.store(false, Ordering::SeqCst);

        // Issue #322: install / clear the auto-mute controller based on
        // the current `mute_output_while_recording` setting. Re-evaluated
        // on each start() so a settings edit picks up on the next
        // worker restart without needing a full app relaunch.
        install_output_mute_from_config();

        // Phase-1 rollout (PR #341 — wiring the Rust audio pipeline):
        // If the user opted in via VOICEPI_AUDIO_BACKEND=rust AND this
        // binary was built with the `audio-in-rust` cargo feature, we
        // splice the Rust capture path into the worker's stdin and ask
        // Python to read frames from there. Default builds and the
        // env-var-unset case go through the exact same code path as
        // before — see `audio_spawn::should_use_rust_audio_backend` for
        // the precise gate. To disable in an emergency: unset the env
        // var, or rebuild without `--features audio-in-rust`.
        // Wave 8 Part 2: `use_rust_audio` used to gate a Rust→Python
        // audio bridge when the user opted in via
        // `VOICEPI_AUDIO_BACKEND=rust`. The bridge only exists so the
        // supervisor could pipe cpal frames into the Python worker's
        // stdin. Wave 8 removed the Python worker; the worker-rust
        // subprocess owns cpal in-process. The env-var warning is
        // still surfaced below so a user with `VOICEPI_AUDIO_BACKEND=rust`
        // in their config sees the "no longer applicable" hint.
        let warn_unavailable = audio_pipeline_requested() && !audio_pipeline_available();
        if warn_unavailable {
            let _ = self.tx.send(RuntimeEvent::Stderr(format!(
                "[runtime] {}",
                audio_spawn::requested_but_unavailable_warning(),
            )));
        }

        self.state = RuntimeState::Starting;
        let mut effective_command = command;

        // Wave 5 PR 6 (+ PR 7 default-flip) of #348: delegate the
        // dictation lifecycle to the `whisper-dictate worker-rust`
        // subprocess on any build compiled with the full feature set
        // (`whisper-rs-local,rust-injection,audio-in-rust,rust-hotkeys`).
        // The subprocess owns the entire lifecycle in-process: it
        // installs its own rdev OS listener, runs the
        // `DictateSession` with the real backends, and emits worker
        // events back to us through stderr (already wired by
        // `stream_lines` below).
        //
        // PR 7 flipped the default: previously the user had to opt IN
        // via `VOICEPI_DICTATE_BACKEND=rust-session`; now the Rust
        // worker runs unconditionally on a full-feature build and the
        // user opts OUT with `VOICEPI_DICTATE_BACKEND=python-legacy`
        // for the Wave-7 → Wave-8 burn-in only. Wave 8 removes the
        // Python bundle and this escape hatch together.
        //
        // When delegating we MUST also skip the supervisor's own
        // in-process Rust hotkey install -- otherwise the parent and
        // the child would both register the same OS chord and race for
        // every press / release. The audio bridge is N/A too (the
        // subprocess opens its own cpal stream via the audio pump in
        // `rust_session_audio.rs`).
        //
        // With the escape hatch set OR with any required feature
        // missing (typical on stock CI builds), this path is a no-op
        // and the supervisor stays on Python. `worker-rust` refuses
        // to run on incomplete-feature builds anyway.
        // Wave 5 PR 7 (#348): the delegate gate is env-only. The Rust
        // worker's `unsupported_worker_rust_settings_reason()` is folded
        // into `should_delegate_to_worker_rust()`, which reads
        // `VOICEPI_STT_BACKEND` directly rather than the saved config --
        // Wave 8 deletes the Python bundle so a per-setting fallback
        // path would just be dead code the moment #348 lands.
        // Codex #441 P2 review round 3: the gate must see the same
        // `VOICEPI_STT_BACKEND` value the child worker will see, which is
        // the merge of `effective_command.env` overrides (materialising
        // AppSettings) on top of the parent process env. Passing the vec
        // in explicitly avoids the false-negative where an upgraded
        // `stt_backend = "parakeet"` config slips past a std::env-only
        // check and the child then falls through to the stub sink.
        // Wave 8 Part 2: Python is gone in v1.20. Delegation is the only
        // valid path -- when the gate declines we cannot fall back to
        // anything, so surface the reason and refuse to spawn.
        // Codex #453 P2: a stale `VOICEPI_DICTATE_BACKEND=python-legacy`
        // no longer blocks delegation (the value has no target); log a
        // one-time hint so upgraded users know to unset it.
        worker_rust::warn_stale_python_legacy_if_set(&effective_command.env);
        let delegate_requested =
            worker_rust::should_delegate_to_worker_rust(&effective_command.env);
        if !delegate_requested {
            self.suspend_session_sink_on_start_failure();
            self.state = RuntimeState::Stopped;
            let reason =
                worker_rust::unsupported_worker_rust_settings_reason(&effective_command.env)
                    .unwrap_or_else(|| {
                        "this build was compiled without the full rust-session feature set \
                 (whisper-rs-local + rust-injection + audio-in-rust + rust-hotkeys); \
                 rebuild with those features to enable dictation"
                            .to_owned()
                    });
            let msg =
                format!("cannot start worker: {reason}. The Python worker was removed in v1.20.");
            let _ = self.tx.send(RuntimeEvent::Error(msg.clone()));
            return Err(anyhow!(msg));
        }
        if let Err(err) = worker_rust::swap_command_to_worker_rust(&mut effective_command) {
            self.suspend_session_sink_on_start_failure();
            self.state = RuntimeState::Stopped;
            let msg = format!(
                "worker-rust delegation failed ({err}); \
                 the Python fallback was removed in v1.20 -- \
                 reinstall whisper-dictate or point VOICEPI_APP_ROOT at a full-feature build"
            );
            let _ = self.tx.send(RuntimeEvent::Error(msg.clone()));
            return Err(anyhow!(msg));
        }
        // Wave 8 Part 2: delegation is the ONLY path now (Python is gone
        // in v1.20). The worker-rust subprocess owns the entire audio
        // pipeline in-process via its own cpal stream, so the parent
        // supervisor MUST NOT open its own Rust audio bridge -- that
        // would race the subprocess for the microphone. Force
        // `use_rust_audio` off here so the downstream `Stdio::piped()`
        // stdin is used only for the supervisor-side clean-shutdown
        // signal (close pipe → child sees EOF and exits), NOT for the
        // Rust audio-frame writer.
        let delegate_to_worker_rust = true;
        let use_rust_audio = false;
        let _ = delegate_requested;

        // P2 #346 finding 1 / P1 #373: wire the Rust hotkey installer on the
        // first start() call when the user has opted in via
        // `VOICEPI_HOTKEY_BACKEND=rust`. Only installed once — the rdev
        // listener thread cannot be cleanly stopped so subsequent restart()
        // calls reuse the existing handle.
        //
        // Python stays enabled when the action sink is the logger sink:
        // the coordinator only logs `[hotkey]` lines and no IPC drives the
        // worker's actual recording lifecycle, so Python must keep
        // listening or PTT goes silent. When the user opts into the
        // session sink (`VOICEPI_DICTATE_BACKEND=rust-session`) AND that
        // install succeeds, the Rust process now drives the dictation
        // loop in-process so we MUST park the Python listener -- otherwise
        // both backends react to the same chord and produce duplicate /
        // conflicting state transitions. Codex P2 #416 runtime.rs:1427.
        // Wave 8 Part 2: the worker-rust subprocess owns its own rdev
        // OS hotkey listener + in-process DictateSession, so the
        // supervisor MUST NOT install a second listener (it would race
        // the subprocess for the same OS chord). The pre-v1.20
        // logger/session-sink installer paths (and the restart-path
        // resume) are gone with the Python worker.
        let _ = &delegate_to_worker_rust;

        let display = effective_command.display();
        let mut process = Command::new(&effective_command.program);
        process
            .args(&effective_command.args)
            .current_dir(&effective_command.working_dir)
            .env(WORKER_EVENTS_ENV, "1")
            .envs(
                effective_command
                    .env
                    .iter()
                    .map(|(key, value)| (key, value)),
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if use_rust_audio || delegate_to_worker_rust {
            // Two callers need a piped stdin:
            //
            // 1. `use_rust_audio`: the audio bridge writes JSON frame
            //    events to the worker's stdin.
            // 2. `delegate_to_worker_rust` (PR #441 review round 2,
            //    Codex P1 finding 3): `worker-rust` unconditionally
            //    reads stdin in its command loop. On a Windows GUI
            //    launch the parent has no console stdin, so
            //    inheriting would leave the child with an invalid
            //    handle -- the read errors immediately and the
            //    worker exits before rdev can drive PTT. Piping
            //    (without ever writing) parks the worker's main
            //    thread on a blocked `read()`; closing the pipe at
            //    supervisor shutdown yields EOF and the worker
            //    exits cleanly.
            //
            // Piping unused stdin can confuse some libraries that
            // probe `isatty()` on launch; neither the Rust audio
            // bridge nor `worker-rust` does that, so this is safe on
            // both branches.
            process.stdin(Stdio::piped());
        }
        // Wave 8 Part 2: `configure_piped_python_stdio` used to set
        // `PYTHONUTF8=1` + `PYTHONIOENCODING=utf-8` on the spawned Python
        // worker so `ensure_ascii=False` JSONL round-tripped cleanly through
        // non-UTF-8 Windows consoles. The child is now the Rust worker-rust
        // subprocess (same binary as the parent) which writes UTF-8 stdio
        // directly, so those env vars are moot and have been dropped.
        configure_background_process(&mut process);
        let mut child = match process.spawn() {
            Ok(c) => c,
            Err(err) => {
                // Codex P2 #416 (round 2) runtime.rs:504 -- the
                // session sink was registered above; without this
                // cleanup PTT would still drive DictateSession after
                // a spawn failure.
                self.suspend_session_sink_on_start_failure();
                self.state = RuntimeState::Stopped;
                return Err(err.into());
            }
        };

        // Iteration-3 review finding #1: when the rust-audio backend
        // is active we need to know when the worker's stdin reader is
        // live so we can defer opening cpal until then (no pre-ready
        // frames piling up in the OS pipe buffer). The Python worker
        // emits `state=ready` on its stderr after model load and right
        // before constructing `Dictate` (which spawns the stdin reader
        // in __init__). Wire the stderr streamer to ping a one-shot
        // channel on that event; an on-the-fly ready-watch thread
        // installs the BridgeHandle once it sees the ping.
        #[cfg(feature = "audio-in-rust")]
        let ready_signal: Option<Sender<()>> = if use_rust_audio {
            let (ready_tx, ready_rx) = mpsc::channel::<()>();
            // Bridge prep up-front so a missing Silero ONNX still
            // fails-fast (synchronous Err from `start()`). The cpal
            // stream is NOT opened here — that's deferred to the
            // ready-watch thread below.
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("audio-in-rust: child stdin was not piped"))?;
            let pending = match audio_spawn::prepare_audio_bridge_for_child(stdin) {
                Ok(p) => p,
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Codex P2 #416 (round 2) runtime.rs:504 -- mirror
                    // the spawn-failure cleanup so an audio-pipeline
                    // init failure doesn't leave the session sink
                    // driving DictateSession against a dead worker.
                    self.suspend_session_sink_on_start_failure();
                    self.state = RuntimeState::Stopped;
                    return Err(anyhow!(
                        "failed to prepare Rust audio pipeline: {err}; \
                         unset VOICEPI_AUDIO_BACKEND to fall back to the \
                         Python sounddevice path"
                    ));
                }
            };
            let device = audio_spawn::resolve_audio_device_from_env(&effective_command.env);
            self.spawn_ready_watch(pending, device, ready_rx);
            Some(ready_tx)
        } else {
            None
        };
        #[cfg(not(feature = "audio-in-rust"))]
        let ready_signal: Option<Sender<()>> = None;

        // Codex P2 (runtime.rs:2074, PR #440) — capture the
        // observer generation for THIS worker instance. Stale readers
        // from a previous child will hold their old generation, so
        // their late-arriving worker events no-op instead of poisoning
        // the fresh controller. stdout doesn't emit worker events so
        // it doesn't need a generation.
        let mute_generation = crate::output_mute::session::current_generation();
        if let Some(stdout) = child.stdout.take() {
            stream_lines(
                stdout,
                self.tx.clone(),
                RuntimeStream::Stdout,
                self.repaint_notifier.clone(),
                None,
                None,
            );
        }
        if let Some(stderr) = child.stderr.take() {
            stream_lines(
                stderr,
                self.tx.clone(),
                RuntimeStream::Stderr,
                self.repaint_notifier.clone(),
                ready_signal,
                Some(mute_generation),
            );
        }

        self.state = RuntimeState::Running;
        self.child = Some(child);
        let _ = self.tx.send(RuntimeEvent::Started { command: display });
        if let Some(notifier) = self.repaint_notifier.as_ref() {
            notifier();
        }
        Ok(())
    }

    /// Iteration-3 review finding #1: park the prepared [`PendingBridge`]
    /// on a background thread that waits for the Python worker's
    /// `state=ready` event before opening cpal and starting the writer.
    /// This avoids the race where the supervisor would otherwise emit
    /// VAD-detected speech frames into the child's stdin DURING the
    /// child's model load (the Python reader doesn't exist until
    /// `Dictate.__init__`, which runs after the model is ready).
    ///
    /// On `ready_rx` ping: open cpal, install the live `BridgeHandle`
    /// into `self.audio_bridge`, and start the error-watcher. If the
    /// receiver hangs up before a ping (worker died / supervisor
    /// stopped during model load), the `PendingBridge` is dropped here
    /// — no cpal stream is ever opened, preserving the user's
    /// mic-permission state.
    #[cfg(feature = "audio-in-rust")]
    fn spawn_ready_watch(
        &self,
        pending: crate::audio::PendingBridge,
        device: String,
        ready_rx: Receiver<()>,
    ) {
        let tx = self.tx.clone();
        let notifier = self.repaint_notifier.clone();
        let bridge_slot = self.audio_bridge.clone();
        let terminal = self.bridge_terminal.clone();
        let cancel = self.bridge_cancel.clone();
        thread::spawn(move || {
            if ready_rx.recv().is_err() {
                // Worker died (or `stop()` torpedoed the streamer)
                // before emitting ready. Drop the pending bridge —
                // cpal never opened, so the user's mic-permission
                // state is preserved for the next run.
                let _ = tx.send(RuntimeEvent::Stderr(
                    "[runtime] audio-in-rust: worker exited before ready; \
                     bridge cancelled (no cpal stream opened)"
                        .to_owned(),
                ));
                drop(pending);
                return;
            }
            // Race-guard: if `stop()` ran while we were parked on the
            // ready signal, don't open cpal — the supervisor is on its
            // way down and doesn't want the bridge any more.
            if cancel.load(Ordering::SeqCst) {
                drop(pending);
                return;
            }
            match pending.start(&device) {
                Ok((bridge, errors)) => {
                    // Recheck the cancel flag AFTER cpal opened. If
                    // `stop()` raced us between the load above and
                    // here, the supervisor already locked the bridge
                    // slot and moved on with nothing to teardown.
                    // Drop the freshly-built handle ourselves to close
                    // cpal + the writer instead of installing it into
                    // a stopped supervisor.
                    if cancel.load(Ordering::SeqCst) {
                        drop(bridge);
                        return;
                    }
                    if let Ok(mut slot) = bridge_slot.lock() {
                        *slot = Some(bridge);
                    }
                    let _ = tx.send(RuntimeEvent::Stderr(
                        "[runtime] audio-in-rust: Rust capture pipeline active for this run"
                            .to_owned(),
                    ));
                    Self::run_bridge_error_loop(errors, tx, notifier, terminal);
                }
                Err(err) => {
                    // cpal open failed (mic in use / unplugged) AFTER
                    // the worker came up ready. Surface as an Error
                    // event and flag the supervisor for teardown so the
                    // UI stops claiming we're recording. Same teardown
                    // path as a runtime bridge error.
                    let _ = tx.send(RuntimeEvent::Error(format!(
                        "audio-in-rust: failed to open capture stream: {err}; \
                         unset VOICEPI_AUDIO_BACKEND to fall back"
                    )));
                    terminal.store(true, Ordering::SeqCst);
                    if let Some(notifier) = notifier.as_ref() {
                        notifier();
                    }
                }
            }
        });
    }

    /// Watch the audio bridge's error channel in a background thread
    /// and translate any [`crate::audio::BridgeError`] into a
    /// [`RuntimeEvent::Error`] (UI surfaces it) or a stderr trace
    /// (expected `WorkerClosed` on PTT release). Stops as soon as the
    /// bridge's channel closes — the bridge sends AT MOST ONE error
    /// per run, so this watcher is a tight one-shot loop. Fire-and-
    /// forget: dropping the bridge closes the channel and the watcher
    /// exits naturally.
    #[cfg(feature = "audio-in-rust")]
    #[allow(dead_code)] // Retained for tests; production path uses run_bridge_error_loop directly.
    fn spawn_audio_bridge_error_watch(
        &self,
        errors: std::sync::mpsc::Receiver<crate::audio::BridgeError>,
    ) {
        let tx = self.tx.clone();
        let notifier = self.repaint_notifier.clone();
        let terminal = self.bridge_terminal.clone();
        // Flag-up the active backend on the supervisor channel so the
        // user can tell from the runtime log which path actually ran.
        let _ = tx.send(RuntimeEvent::Stderr(
            "[runtime] audio-in-rust: Rust capture pipeline active for this run".to_owned(),
        ));
        thread::spawn(move || {
            Self::run_bridge_error_loop(errors, tx, notifier, terminal);
        });
    }

    /// Pure error-translation loop. Extracted so the iteration-3
    /// ready-watch thread (which already owns the bridge-creation
    /// site) can drive it inline without spawning a second thread —
    /// and so unit tests can drive it without a real bridge.
    #[cfg(feature = "audio-in-rust")]
    fn run_bridge_error_loop(
        errors: std::sync::mpsc::Receiver<crate::audio::BridgeError>,
        tx: Sender<RuntimeEvent>,
        notifier: Option<RepaintNotifier>,
        terminal: Arc<AtomicBool>,
    ) {
        use crate::audio::BridgeError;
        while let Ok(err) = errors.recv() {
            // Iteration-2 review finding #3: `Io` and `Pipeline` are
            // TERMINAL — the writer has already dropped the child's
            // stdin handle and exited, so even if the worker is
            // technically still alive it can no longer receive audio.
            // Raise the teardown flag so the next `poll()` kills the
            // child and surfaces an `Exited`, flipping the UI back to
            // Stopped instead of leaving the user staring at a
            // running-but-deaf worker.
            let is_terminal = matches!(err, BridgeError::Io(_) | BridgeError::Pipeline(_));
            let event = match err {
                // WorkerClosed = the Python child closed stdin (normal
                // teardown). Surface as a stderr trace, not an Error
                // event, so the UI doesn't pop a false-positive failure
                // banner on every PTT release.
                BridgeError::WorkerClosed => RuntimeEvent::Stderr(
                    "[runtime] audio-in-rust: worker closed audio stdin (normal teardown)"
                        .to_owned(),
                ),
                BridgeError::Io(msg) => RuntimeEvent::Error(format!(
                    "audio-in-rust: failed writing to worker stdin: {msg}"
                )),
                BridgeError::Pipeline(msg) => {
                    RuntimeEvent::Error(format!("audio-in-rust: capture pipeline error: {msg}"))
                }
            };
            let _ = tx.send(event);
            if is_terminal {
                terminal.store(true, Ordering::SeqCst);
            }
            if let Some(notifier) = notifier.as_ref() {
                notifier();
            }
        }
    }

    pub fn stop(&mut self) -> Result<()> {
        // Stop the Rust audio bridge BEFORE killing the worker. The
        // bridge closes its end of the stdin pipe; the worker's
        // RustStdinAudioSource sees EOF and finishes the current
        // utterance (no half-buffered audio). If we killed the worker
        // first the bridge would race the kill with a write and emit
        // a spurious `WorkerClosed` event.
        #[cfg(feature = "audio-in-rust")]
        {
            // Iteration-3 race-guard: tell any in-flight ready-watch
            // thread to abort BEFORE we look at the slot. If it's
            // mid-`pending.start()`, it'll see this and drop the
            // freshly-built handle on completion instead of
            // installing it into a stopped supervisor.
            self.bridge_cancel.store(true, Ordering::SeqCst);
            if let Some(mut bridge) = self.audio_bridge.lock().ok().and_then(|mut s| s.take()) {
                bridge.stop();
            }
        }

        // Wave 8 Part 2: the parent supervisor no longer installs a
        // Rust hotkey listener -- the worker-rust subprocess owns the
        // rdev listener and the DictateSession in-process, and its
        // own drop path suspends the listener when the subprocess
        // exits. The previous `self.hotkey_handle.suspend()` guard
        // (Fix 4 #373) is therefore a no-op.

        // Codex P1 (runtime.rs:458, PR #440): every teardown path must
        // clear the process-global auto-mute controller. `install()`
        // only runs on `Runtime::start`, so without this the controller
        // would remain parked in its `Recording` phase after a stop and
        // leave the user's speakers muted until the next start.
        clear_output_mute_on_teardown();

        let Some(mut child) = self.child.take() else {
            self.state = RuntimeState::Stopped;
            return Ok(());
        };

        self.state = RuntimeState::Stopped;
        let tx = self.tx.clone();
        let notifier = self.repaint_notifier.clone();
        thread::spawn(move || {
            let result = kill_child(&mut child).and_then(|_| child.wait().map_err(Into::into));
            match result {
                Ok(status) => {
                    let _ = tx.send(RuntimeEvent::Exited {
                        code: status.code(),
                    });
                }
                Err(err) => {
                    let _ = tx.send(RuntimeEvent::Error(format!("stop failed: {err}")));
                }
            }
            if let Some(notifier) = notifier.as_ref() {
                notifier();
            }
        });
        Ok(())
    }

    pub fn restart(&mut self, command: WorkerCommand) -> Result<()> {
        self.stop()?;
        self.start(command)
    }

    /// Test-only hook (crate-visible): simulate the bridge-error
    /// watcher observing a terminal `BridgeError::Io` /
    /// `BridgeError::Pipeline`. The next `poll()` will then run the
    /// iteration-2 review finding #3 teardown path (kill the child,
    /// drop the bridge, synthesize `Exited`). Crate-visible so
    /// `runtime/bridge_terminal_tests.rs` can exercise the path
    /// without spinning up a real cpal failure.
    #[cfg(all(test, feature = "audio-in-rust"))]
    #[doc(hidden)]
    pub(crate) fn trigger_bridge_terminal_for_tests(&self) {
        self.bridge_terminal.store(true, Ordering::SeqCst);
    }

    /// No-op preserved for call-site compatibility.
    ///
    /// Wave 8 Part 2: the parent supervisor no longer holds a
    /// `HotkeyHandle`; the worker-rust subprocess owns the rdev
    /// listener and the `DictateSession` in-process. On child exit
    /// the subprocess's own drop path suspends its listener. Kept as
    /// a stub so the existing call sites (`stop`, `poll`,
    /// `bridge_terminal` teardown) don't need to be edited on every
    /// review round.
    fn suspend_session_sink_on_exit(&self) {}

    /// Same story as [`Self::suspend_session_sink_on_exit`] for the
    /// `start()` error-return paths.
    fn suspend_session_sink_on_start_failure(&self) {}

    /// Drain pending [`external_toggle::ExternalCommand`]s from the
    /// process-global channel (filled by the Unix signal handler installed
    /// in [`external_toggle::install_signal_handlers`] and by the CLI
    /// forwarder when it sends SIGUSR1/SIGUSR2). Each command is routed to
    /// the active hotkey coordinator so the same `Stage::Idle` /
    /// `Stage::Recording` / `Stage::Processing` guards apply uniformly,
    /// regardless of whether the trigger came from a keyboard chord or a
    /// compositor keybinding.
    ///
    /// When the Rust hotkey coordinator is not installed (e.g. the user is
    /// on the Python backend, the default in most builds), each command
    /// surfaces as a stderr event so the user sees the trigger arrived and
    /// understands why nothing happened. Issue #326.
    pub fn dispatch_external_commands(&self) {
        for cmd in external_toggle::take_pending_commands() {
            self.dispatch_external_command(cmd);
        }
    }

    /// Single-shot variant of [`Self::dispatch_external_commands`] —
    /// exposed for tests that synthesise commands directly without going
    /// through the channel.
    ///
    /// Wave 8 Part 2: the coordinator now lives in the worker-rust
    /// subprocess, so the supervisor cannot dispatch events into it
    /// directly. Until issue #326's parent<->child IPC is wired, the
    /// command surfaces as a runtime-event stderr line so the user can
    /// see the trigger arrived and the follow-up is documented.
    pub fn dispatch_external_command(&self, cmd: external_toggle::ExternalCommand) {
        let _ = self.tx.send(RuntimeEvent::Stderr(format!(
            "[external-toggle] received {cmd:?}; coordinator lives in the worker-rust \
             subprocess (issue #326 follow-up will wire a parent<->child IPC)"
        )));
    }

    pub fn poll(&mut self) -> Vec<RuntimeEvent> {
        // Issue #326: drain any external triggers (SIGUSR1/SIGUSR2 or
        // `whisper-dictate --toggle-recording`) BEFORE the regular
        // child-status check so the resulting events land in this poll's
        // output vec.
        self.dispatch_external_commands();
        // Iteration-2 review finding #3: act on a terminal bridge error
        // BEFORE the regular try_wait. The bridge watcher has already
        // emitted a `RuntimeEvent::Error` describing the failure; here
        // we follow up with the teardown the watcher couldn't perform
        // from a background thread (it has no `&mut self`). Kill the
        // child, drop the bridge handle, and synthesize an `Exited`
        // so the UI flips back to Stopped on its next poll.
        #[cfg(feature = "audio-in-rust")]
        if self.bridge_terminal.swap(false, Ordering::SeqCst) {
            self.bridge_cancel.store(true, Ordering::SeqCst);
            if let Some(mut bridge) = self.audio_bridge.lock().ok().and_then(|mut s| s.take()) {
                bridge.stop();
            }
            if let Some(mut child) = self.child.take() {
                let _ = kill_child(&mut child);
                let exit_code = child.wait().ok().and_then(|status| status.code());
                self.state = RuntimeState::Stopped;
                // Codex P2 #416 (round 2) runtime.rs:875 -- the
                // try_wait arms call this on every unexpected exit;
                // the bridge-terminal branch kills+takes the child
                // and returns BEFORE those arms run, so without this
                // call a terminal BridgeError would leave the
                // session sink driving DictateSession while the UI
                // shows Stopped.
                self.suspend_session_sink_on_exit();
                // Codex P1 (runtime.rs:458, PR #440): clear the
                // process-global auto-mute controller so the user's
                // speakers are restored even on a bridge-terminal
                // teardown.
                clear_output_mute_on_teardown();
                let _ = self.tx.send(RuntimeEvent::Exited { code: exit_code });
                if let Some(notifier) = self.repaint_notifier.as_ref() {
                    notifier();
                }
            } else {
                // No child to kill (already exited): just be sure
                // state reflects stopped.
                self.state = RuntimeState::Stopped;
                // Same Codex P2 (round 2) -- even if the child was
                // already gone, the hotkey handle could still be
                // live.
                self.suspend_session_sink_on_exit();
                // Same Codex P1 (PR #440) — mute controller could
                // still be installed even if the child is gone.
                clear_output_mute_on_teardown();
            }
            return self.rx.try_iter().collect();
        }

        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.state = RuntimeState::Stopped;
                    self.child = None;
                    // The worker exited (crash, --doctor finished,
                    // user-killed, …). Tear the audio bridge down so
                    // it doesn't keep cpal open against a missing
                    // reader. Same teardown order as `stop()`.
                    #[cfg(feature = "audio-in-rust")]
                    {
                        self.bridge_cancel.store(true, Ordering::SeqCst);
                        if let Some(mut bridge) =
                            self.audio_bridge.lock().ok().and_then(|mut s| s.take())
                        {
                            bridge.stop();
                        }
                    }
                    self.suspend_session_sink_on_exit();
                    // Codex P1 (runtime.rs:458, PR #440): unexpected-exit
                    // path also needs to drop the mute controller so a
                    // worker crash while muted leaves the speakers
                    // restored.
                    clear_output_mute_on_teardown();
                    let _ = self.tx.send(RuntimeEvent::Exited {
                        code: status.code(),
                    });
                }
                Ok(None) => {}
                Err(err) => {
                    self.state = RuntimeState::Stopped;
                    self.child = None;
                    #[cfg(feature = "audio-in-rust")]
                    {
                        self.bridge_cancel.store(true, Ordering::SeqCst);
                        if let Some(mut bridge) =
                            self.audio_bridge.lock().ok().and_then(|mut s| s.take())
                        {
                            bridge.stop();
                        }
                    }
                    self.suspend_session_sink_on_exit();
                    // Same Codex P1 (PR #440) — try_wait error path.
                    clear_output_mute_on_teardown();
                    let _ = self.tx.send(RuntimeEvent::Error(err.to_string()));
                }
            }
        }

        self.rx.try_iter().collect()
    }
}

impl Drop for RuntimeSupervisor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

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

// Wave 8 Part 2: the historic `Platform`, `PlannedCommand`, `InstallPlan`,
// `pip_install_command`, `run_install_command`, `wants_cuda_runtime`,
// `requirements_path`, `first_existing_requirements`, `python_source_root`
// helpers were the machinery behind `whisper-dictate install`: create a
// Python virtual environment under `~/.venv-whisper-dictate`, upgrade
// pip, and run `pip install -r requirements/{cpu,gpu}.txt` inside it.
// v1.20 dropped the entire Python bundle in favour of the in-process
// Rust worker, so the whole install pipeline (venv creation, pip
// invocation, requirements resolution, per-platform venv layout,
// Python-source-root marker) is dead code. The `install` subcommand
// itself is now a graceful no-op (see [`install`] above).

/// Build the base [`WorkerCommand`] the supervisor uses to spawn the
/// in-process Rust dictation worker. `RuntimeSupervisor::start`
/// unconditionally swaps `program` + `args` to
/// `<current-exe> worker-rust` via
/// [`worker_rust::swap_command_to_worker_rust`], so the seed values
/// here are effectively placeholders -- the `env` vector is the only
/// field the swap preserves, and that is what materialises the
/// AppSettings-derived overrides (`config::worker_env_overrides()`)
/// onto the child process.
///
/// Wave 8 Part 2 rewrite: the previous version built a Python `-m
/// whisper_dictate.runtime` invocation with `PYTHONPATH`,
/// `VOICEPI_RUST_INJECTOR`, and a pynput-hotkey-name normaliser. All
/// three exist only because the Python worker consumed them; v1.20
/// dropped the Python bundle, and the Rust worker reads its
/// configuration directly from `VOICEPI_*` env vars (which
/// `worker_env_overrides` already sets).
pub fn worker_command(app_root: impl AsRef<Path>) -> WorkerCommand {
    worker_command_with_args(app_root, Vec::<String>::new())
}

/// Same as [`worker_command`] but allows the caller to append
/// passthrough args to the seed command. The passthrough args are
/// dropped by the delegation swap in [`RuntimeSupervisor::start`], so
/// this function is really just a legacy shim that keeps the
/// UI/CLI call sites compiling until they're refactored to build a
/// direct `worker-rust` command themselves.
pub fn worker_command_with_args(
    app_root: impl AsRef<Path>,
    passthrough_args: impl IntoIterator<Item = String>,
) -> WorkerCommand {
    let app_root = app_root.as_ref().to_path_buf();
    let program = env::current_exe().unwrap_or_else(|_| PathBuf::from("whisper-dictate"));
    let args: Vec<String> = passthrough_args.into_iter().collect();
    let env: Vec<(String, String)> = config::worker_env_overrides();
    WorkerCommand {
        program,
        args,
        working_dir: app_root,
        env,
    }
}

/// Parse a `VOICEPI_TOGGLE` env-var value to a boolean. Accepts the same
/// set of truthy strings that the Python config layer and shell tooling use:
/// `"true"`, `"1"`, `"yes"`, `"on"` — all case-insensitive. Returns `false`
/// for any other value, including empty string.
///
/// Extracted as a standalone helper so it can be unit-tested independently
/// of the full command-build pipeline (Codex P2 finding, PR #373).
pub(crate) fn parse_toggle_value(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

/// Extract PTT key names from a [`WorkerCommand`]'s environment: split
/// `VOICEPI_KEY` on `+`, trim whitespace, drop empty segments. Returns an
/// empty `Vec` when the env var is absent or blank — callers use this as a
/// "no hotkey configured" sentinel.
pub(crate) fn extract_hotkey_key_names(command: &WorkerCommand) -> Vec<String> {
    command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_KEY")
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

// Wave 8 Part 2: the parent supervisor used to install a Rust hotkey
// listener directly whenever `VOICEPI_HOTKEY_BACKEND=rust` was set --
// the coordinator either fired stderr `[hotkey]` log lines
// (logger sink) or drove an in-process `DictateSession` (session
// sink). Both paths existed to work around the Python worker owning
// dictation; v1.20 dropped the Python worker and made worker-rust
// delegation mandatory, so the whole hotkey install pipeline
// (`RestartHotkeyDecision`, `restart_hotkey_decision`,
// `rust_hotkey_backend_requested/active`, `maybe_install_rust_hotkey`,
// `install_rust_hotkey_from_command`, `install_logger_sink_hotkey`,
// `install_session_sink_hotkey`) is dead in the parent -- the
// worker-rust subprocess owns rdev + the DictateSession in-process.
//
// The dispatch surface that external-toggle / signal-hook still
// consumes is preserved in [`RuntimeSupervisor::dispatch_external_command`];
// it degrades to a stderr line explaining the coordinator lives in
// the worker-rust child now (issue #326 follow-up will wire an IPC).

pub fn default_worker_command() -> WorkerCommand {
    worker_command(app_root())
}

pub fn default_worker_command_with_args(args: Vec<String>) -> WorkerCommand {
    worker_command_with_args(app_root(), args)
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
/// build returns false even if the env var is set.
pub fn audio_pipeline_available() -> bool {
    cfg!(feature = "audio-in-rust")
}

/// The app root used to resolve bundled resources (e.g. `benchmark/corpus.json`).
/// Public wrapper over the internal resolver so the UI can read the SAME corpus
/// manifest the worker does, honoring `VOICEPI_APP_ROOT` / the installed layout.
pub fn resource_app_root() -> PathBuf {
    app_root()
}

// Wave 8 Part 2: `doctor_command`, `benchmark_command`, `windows_command`,
// `audio_devices_command`, `test_audio_device_command`,
// `record_corpus_item_command`, `propagate_rust_devices_backend`,
// `install_command`, `install_command_from_exe` were all thin builders
// around the Python worker's flag-based dispatch
// (`python -m whisper_dictate.runtime --doctor` etc.). v1.20 removed
// the Python worker; the corresponding CLI subcommands
// (`whisper-dictate doctor`, `bench`, `corpus-record ...`) now surface
// a "moved-to-worker-rust" message in-process rather than spawning a
// subprocess. The UI tasks that used these builders were removed at
// the same time. Track `#348` follow-ups for the Rust replacements.

fn configure_background_process(_command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        _command.creation_flags(CREATE_NO_WINDOW);
    }
}

#[derive(Debug, Clone, Copy)]
enum RuntimeStream {
    Stdout,
    Stderr,
}

/// `mute_observer_generation` — Codex P2 (runtime.rs:2074, PR #440):
/// observer generation captured when this reader is created. `None` on
/// streams that never emit worker events (e.g. stdout). See
/// [`crate::output_mute::session::current_generation`].
fn stream_lines<R>(
    reader: R,
    tx: Sender<RuntimeEvent>,
    stream: RuntimeStream,
    repaint_notifier: Option<RepaintNotifier>,
    ready_signal: Option<Sender<()>>,
    mute_observer_generation: Option<u64>,
) where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let mut ready_signal = ready_signal;
        for line in BufReader::new(reader).lines() {
            match line {
                Ok(line) => {
                    let event = runtime_event_from_line(line, stream);
                    // Iteration-3 review finding #1: when the rust-audio
                    // backend is active, fire the one-shot ready signal
                    // the moment we see the worker's first ready event.
                    // The supervisor's ready-watch thread is parked on
                    // the matching receiver; it will then open cpal and
                    // start producing frames into the now-live Python
                    // stdin reader.
                    if let RuntimeEvent::Worker(worker) = &event {
                        if worker.state.as_deref() == Some("ready") {
                            if let Some(signal) = ready_signal.take() {
                                let _ = signal.send(());
                            }
                        }
                        // Issue #322: fan every worker-state transition
                        // into the auto-mute observer. Cheap no-op when
                        // the `mute_output_while_recording` setting is
                        // off (no controller installed), so leaving this
                        // permanently in the hot path is safe.
                        //
                        // Codex P2 (runtime.rs:2074 + state.rs:158,
                        // PR #440) — passes the generation captured at
                        // reader creation so a stale reader from a
                        // stopped child no-ops, and surfaces any backend
                        // failure through the runtime log so the user
                        // sees when the mute silently didn't happen.
                        if let Some(gen) = mute_observer_generation {
                            if let Some(err) = crate::output_mute::session::observe_worker_state(
                                worker.state.as_deref(),
                                gen,
                            ) {
                                let _ = tx.send(RuntimeEvent::Stderr(format!(
                                    "[output-mute] backend failure while observing state {state:?}: {err}",
                                    state = worker.state.as_deref().unwrap_or(""),
                                )));
                            }
                        }
                        // PR #441 review round 2 (Codex P1 finding 4):
                        // the delegated `worker-rust` subprocess emits
                        // `state=error` with `reason=hotkey_install_failed`
                        // when rdev refuses to install (missing display,
                        // permission denied, unsupported chord, ...).
                        // Surface a prominent stderr diagnostic so users
                        // see a clear "PTT will not work" hint even if
                        // the worker's raw stderr line was lost in the
                        // noise. Wave 8 removed the Python fallback, so
                        // the diagnostic now points to the ways to fix
                        // the underlying rdev refusal rather than to an
                        // escape hatch that no longer exists.
                        if worker.state.as_deref() == Some("error")
                            && worker.payload.get("reason").and_then(|v| v.as_str())
                                == Some("hotkey_install_failed")
                        {
                            let detail = worker
                                .payload
                                .get("detail")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(no detail)");
                            let _ = tx.send(RuntimeEvent::Stderr(format!(
                                "[runtime] worker-rust hotkey install failed ({detail}) -- \
                                 PTT will not work in this session. Check that a display is \
                                 attached, that accessibility permission is granted (macOS), \
                                 and that the configured chord is one the rdev backend supports."
                            )));
                        }
                        // Issue #324: persist one row per accepted
                        // utterance into the per-user SQLite history
                        // store. Best-effort and feature-gated -- the
                        // wrapper logs failures and swallows them so
                        // a DB hiccup (locked file, disk full) never
                        // breaks the dictation pipeline.
                        #[cfg(feature = "history-sqlite")]
                        if worker.event == "utterance" {
                            crate::history::try_record_utterance_default(&worker.payload);
                        }
                    }
                    let _ = tx.send(event);
                    if let Some(notifier) = repaint_notifier.as_ref() {
                        notifier();
                    }
                }
                Err(err) => {
                    let _ = tx.send(RuntimeEvent::Error(err.to_string()));
                    if let Some(notifier) = repaint_notifier.as_ref() {
                        notifier();
                    }
                    break;
                }
            }
        }
    });
}

fn runtime_event_from_line(line: String, stream: RuntimeStream) -> RuntimeEvent {
    if matches!(stream, RuntimeStream::Stderr) {
        if let Some(worker_event) = parse_worker_event(&line) {
            return RuntimeEvent::Worker(worker_event);
        }
    }

    match stream {
        RuntimeStream::Stdout => RuntimeEvent::Stdout(line),
        RuntimeStream::Stderr => RuntimeEvent::Stderr(line),
    }
}

fn parse_worker_event(line: &str) -> Option<WorkerEvent> {
    let raw = line.strip_prefix(WORKER_EVENT_PREFIX)?;
    let payload: Value = serde_json::from_str(raw).ok()?;
    let event = payload.get("event")?.as_str()?.to_owned();
    let state = payload
        .get("state")
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    Some(WorkerEvent {
        event,
        state,
        payload,
    })
}

/// Read the user's current `mute_output_while_recording` setting from
/// config.json (falling back to the [`crate::output_mute::session::MUTE_OUTPUT_ENV`]
/// env var when set) and install (or clear) the process-global
/// auto-mute controller accordingly (issue #322). Called from
/// `Runtime::start` so a settings edit is picked up on the next worker
/// restart.
///
/// A missing / unreadable config file is treated as "toggle off" — we
/// never want to silently start muting the user's audio because we
/// could not load their preferences.
///
/// Codex P2 (runtime.rs:2060, PR #440) — env-var precedence is now the
/// same as the schema-derived worker overrides: an explicit
/// `VOICEPI_MUTE_OUTPUT_WHILE_RECORDING=1` in the environment installs
/// the controller even when config.json is silent or set to `false`,
/// matching the schema's documented env-fallback semantics.
fn install_output_mute_from_config() {
    // Codex P2 (session.rs:130, PR #440) — pass Option<bool> so the
    // session installer can distinguish "user explicitly set this in
    // config.json" from "key is missing, fall back to env or default".
    // A missing config file, an unreadable JSON payload, or a key that
    // is absent all resolve to None so the env var can still take
    // effect. `load_raw_config` returns an empty object for a
    // nonexistent file (not an error), so a genuine parse failure is
    // the only case where None comes from the Err path.
    //
    // Codex P2 (session.rs:130, PR #443) — the parse helper now lives
    // in `output_mute::session` so the UI Reload path in
    // `settings_state.rs` resolves the exact same config-vs-env
    // precedence (see `read_mute_key_from_json`).
    let config_value = config::load_raw_config().ok().and_then(|value| {
        value
            .as_object()
            .and_then(crate::output_mute::session::read_mute_key_from_json)
    });
    crate::output_mute::session::install_from_settings(config_value);
}

/// Clear the process-global auto-mute controller on every worker
/// teardown path (Codex P1 runtime.rs:458, PR #440).
///
/// `install_output_mute_from_config` only runs on `Runtime::start`, so
/// a worker stop/kill/crash used to leave the controller parked in its
/// `Recording` phase indefinitely — the controller's `Drop` restore
/// never fired, and the user's speakers could stay muted until either
/// another start replaced the controller or the app exited entirely.
/// Dropping the controller via `install(false)` triggers `Drop` which
/// runs the pending restore. Safe to call unconditionally: when no
/// controller is installed (the setting is off) this is a mutex-lock +
/// no-op.
fn clear_output_mute_on_teardown() {
    crate::output_mute::session::install(false);
}

fn kill_child(child: &mut Child) -> Result<()> {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let mut command = Command::new("taskkill");
        command
            .args(["/PID", &pid, "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_background_process(&mut command);
        let status = command.status();
        if status.as_ref().is_ok_and(|s| s.success()) {
            return Ok(());
        }
    }

    child.kill()?;
    Ok(())
}

/// Resolve the user's home directory. Preserved from the pre-Wave-8
/// installer helpers because [`install_linux_desktop_entries`] still
/// needs to write `~/.local/share/applications/whisper-dictate.desktop`.
fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Resolve the app root. Preferred order:
///
/// 1. `VOICEPI_APP_ROOT` override.
/// 2. The directory containing the current executable (production
///    installs).
/// 3. The workspace root, computed from `CARGO_MANIFEST_DIR` (`cargo
///    test`/`cargo run` from a checkout).
///
/// Wave 8 Part 2 (Codex #453 P2): the resolver replaced the pre-v1.20
/// `src/python/whisper_dictate/runtime.py` Python-source marker with a
/// native-bundle marker (`VERSION` file next to the binary in every
/// packaged install: chocolatey nupkg, Inno setup, Linux tarball, nix
/// derivation). Without a marker check `cargo run` / `cargo test` /
/// integration-test builds would return `target/debug` (or
/// `target/release`) as the app root and the documented fallback to
/// `source_root()` for the shipped resources (`benchmark/corpus.json`,
/// `packaging/linux/ubuntu26.04/setup.sh`, ...) would never fire.
fn app_root() -> PathBuf {
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

/// Bundle-marker check for the exe-based `app_root` resolver. Accepts
/// the exe's parent directory only when it looks like a shipped
/// whisper-dictate install: either a top-level `VERSION` file (chocolatey
/// nupkg, Linux tarball, nix derivation all ship this) OR a
/// `packaging/linux/ubuntu26.04/setup.sh` (source checkout used as the
/// install root via `VOICEPI_APP_ROOT`).
///
/// A binary sitting in `target/debug/` under a checkout has neither, so
/// `app_root` falls through to `source_root()` and the dev workflow
/// (`cargo run -- ui`) keeps resolving `benchmark/corpus.json` /
/// `packaging/linux/ubuntu26.04/setup.sh` from the workspace root.
fn app_root_from_exe_path(exe: &Path) -> Option<PathBuf> {
    let root = exe.parent()?;
    let has_version_marker = root.join("VERSION").is_file();
    let has_packaging_marker = root
        .join("packaging")
        .join("linux")
        .join("ubuntu26.04")
        .join("setup.sh")
        .is_file();
    if has_version_marker || has_packaging_marker {
        Some(root.to_path_buf())
    } else {
        None
    }
}

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

pub mod audio_spawn;
// Issue #327: cross-platform single-instance gate with CLI-arg
// forwarding. Split into `single_instance/{mod,socket,lockfile}.rs`
// so each file stays under the project's 500-LOC modularity ceiling.
// The module ships the machinery; opt-in wire-up lives in
// `main.rs::run()` behind `VOICEPI_SINGLE_INSTANCE`.
pub mod single_instance;
// Wave 5 PR 6+7 of #348: the `whisper-dictate worker-rust` CLI entry
// point + supervisor-side "delegate to worker-rust subprocess" gate.
// After PR 7 (default flip) a full-feature build spawns the Rust
// worker unconditionally; users opt out with
// `VOICEPI_DICTATE_BACKEND=python-legacy` to fall back to the pre-flip
// Python orchestrator during the Wave-7 → Wave-8 burn-in. Wave 8
// removes the Python bundle and this fallback together.
pub mod worker_rust;
// Issue #326: CLI flags + Unix signals (SIGUSR1 = toggle, SIGUSR2 = cancel)
// for compositor-driven external triggers. Lives in its own file so
// `runtime.rs` does not grow past the 500-LOC modularity guideline. See
// the module docs for the wire protocol and the daemon-side install hook.
pub mod external_toggle;
// PR #441 review round 2: env-var gate helpers split out of
// `rust_session_sink` so that file stays under the AGENTS.md
// ~500-LOC-per-file modularity guideline. Owns
// `dictate_backend_python_legacy_requested` /
// `dictate_backend_rust_session_requested`; `rust_session_sink`
// re-exports them so external call sites are unchanged.
pub(crate) mod rust_session_dictate_env;

// Wave 5 PR 4 of #348: opt-in (`VOICEPI_DICTATE_BACKEND=rust-session`)
// wiring that drives a `DictateSession` from the hotkey coordinator's
// action sink. Stays out of the production path until PR 6 flips the
// default. Lives in its own file so `runtime.rs` does not grow past
// the 500-LOC modularity guideline.
pub(crate) mod rust_session_sink;

// Wave 5 PR 5 of #348: real-backend constructor for the session sink.
// Gated on `whisper-rs-local + rust-injection` so default builds compile
// zero new code from this PR. The sink in `rust_session_sink::build_production_sink`
// calls into this module to construct a `DictateSession<WhisperLocalTranscribeBackend,
// ProductionInjectBackend>`; on feature absence OR model-resolution failure it
// falls back to the PR 4 stub session so the wire-up still installs.
#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) mod rust_session_real_backends;

// Wave 5 PR 5 of #348 round 2 (Codex P2 #423 finding 4): production
// `InjectBackend` wrapper that honors `VOICEPI_INJECT_MODE=print`
// (stdout-only dry-run). Modifier release lives inside
// `dictate/backends/inject.rs::EnigoInjectBackend` itself (Codex P2
// #417 inject.rs:110 follow-up in PR #419) so the wrapper just
// delegates for the Enigo arm. Gated on the same feature pair the
// real-backend module requires; without whisper-rs-local nothing
// constructs the wrapper and its items would dead-code.
#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) mod rust_session_inject;

// Wave 5 PR 5 of #348 round 2 (Codex P1 #423 finding 1): audio-pump
// that forwards `AudioPipeline` frames into the real
// `DictateSession`'s `push_frame`. Without this the rust-session path
// captured no audio and every stop hit the `no_audio` early-return.
// Gated on all three features the full real-backend path requires.
#[cfg(all(
    feature = "whisper-rs-local",
    feature = "rust-injection",
    feature = "audio-in-rust"
))]
pub(crate) mod rust_session_audio;

#[cfg(test)]
mod app_root_tests;
#[cfg(test)]
mod audio_backend_tests;
#[cfg(test)]
mod audio_spawn_tests;
#[cfg(all(test, feature = "audio-in-rust"))]
mod bridge_terminal_tests;
// Issue #326: sibling tests for `external_toggle`.
#[cfg(test)]
mod external_toggle_tests;
// Sibling tests for `rust_session_sink` (Wave 5 PR 4 of #348). Split
// across three files to keep each under the ~500-LOC modularity
// guideline (AGENTS.md "Review guidelines", Codex P2 PR #421):
// - `rust_session_sink_tests`: pure helpers + EventForwarder framing.
// - `rust_session_sink_coverage_tests`: Sonar gate-uplift targets.
// - `rust_session_sink_e2e_tests`: synthetic Press/Release/Cancel
//   integration tests through the coordinator + session.
#[cfg(test)]
mod rust_session_sink_coverage_tests;
#[cfg(test)]
mod rust_session_sink_e2e_tests;
#[cfg(test)]
mod rust_session_sink_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod ubuntu_setup_tests;
#[cfg(test)]
mod windows_process_tests;
#[cfg(test)]
mod worker_event_tests;
