use std::env;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
#[cfg(feature = "audio-in-rust")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
#[cfg(feature = "audio-in-rust")]
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::config;

const PYTHON_ENV: &str = "VOICEPI_PYTHON";
const BOOTSTRAP_PYTHON_ENV: &str = "VOICEPI_BOOTSTRAP_PYTHON";
const APP_ROOT_ENV: &str = "VOICEPI_APP_ROOT";
const WORKER_EVENTS_ENV: &str = "VOICEPI_WORKER_EVENTS";
const RUST_INJECTOR_ENV: &str = "VOICEPI_RUST_INJECTOR";
const WORKER_EVENT_PREFIX: &str = "[worker-event] ";
/// Opt-in switch for the experimental Rust-side capture pipeline (cpal +
/// rubato + Silero via vad-rs). Read at supervisor start. Has no effect
/// unless the binary was compiled with the `audio-in-rust` cargo feature
/// AND the env var is set to a truthy value. See
/// [`audio_pipeline_requested`] for the parsing.
pub const AUDIO_BACKEND_ENV: &str = "VOICEPI_AUDIO_BACKEND";
const PYTHON_UTF8_ENV: &str = "PYTHONUTF8";
const PYTHON_IO_ENCODING_ENV: &str = "PYTHONIOENCODING";
const PYTHONPATH_ENV: &str = "PYTHONPATH";
/// Maximum captured worker output kept in memory, measured in UTF-8 bytes
/// (`str::len()`), despite the legacy `_CHARS` suffix in the public name.
pub const CAPTURE_OUTPUT_MAX_CHARS: usize = 200_000;
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

pub fn run_terminal(args: Vec<String>) -> Result<()> {
    let command = default_worker_command_with_args(args);
    run_foreground(&command)
}

pub fn doctor() -> Result<()> {
    run_foreground(&doctor_command())
}

pub fn install() -> Result<()> {
    let plan = InstallPlan::for_current_environment(app_root())?;
    plan.run()
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
    /// Live Rust hotkey handle, installed the first time `start()` is called
    /// when `VOICEPI_HOTKEY_BACKEND=rust` is set AND the binary was built
    /// with the `rust-hotkeys` feature. `None` when the backend is not
    /// requested or the install failed (supervisor stays on pynput then).
    ///
    /// Only installed once per process lifetime: the rdev listener thread
    /// cannot be cleanly stopped, so subsequent `restart()` calls reuse the
    /// existing handle. The coordinator retains its state machine across
    /// restarts — that is fine because the coordinator only cares about
    /// key-press events, not about which Python worker run they arrive in.
    ///
    /// P2 #346 finding 1.
    hotkey_handle: Option<crate::hotkey::HotkeyHandle>,
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
            hotkey_handle: None,
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
        let use_rust_audio = audio_spawn::should_use_rust_audio_backend();
        let warn_unavailable = audio_pipeline_requested() && !audio_pipeline_available();
        if warn_unavailable {
            let _ = self.tx.send(RuntimeEvent::Stderr(format!(
                "[runtime] {}",
                audio_spawn::requested_but_unavailable_warning(),
            )));
        }

        self.state = RuntimeState::Starting;
        let mut effective_command = command;

        // Wave 5 PR 6 of #348: delegate the dictation lifecycle to the
        // `whisper-dictate worker-rust` subprocess when the user opted
        // in via `VOICEPI_DICTATE_BACKEND=rust-session` AND the binary
        // was built with the full feature set
        // (`whisper-rs-local,rust-injection,audio-in-rust,rust-hotkeys`).
        // The subprocess owns the entire lifecycle in-process: it
        // installs its own rdev OS listener, runs the
        // `DictateSession` with the real backends, and emits worker
        // events back to us through stderr (already wired by
        // `stream_lines` below).
        //
        // When delegating we MUST also skip the supervisor's own
        // in-process Rust hotkey install -- otherwise the parent and
        // the child would both register the same OS chord and race for
        // every press / release. The audio bridge is N/A too (the
        // subprocess opens its own cpal stream via the audio pump in
        // `rust_session_audio.rs`).
        //
        // Without the env var OR with any feature missing this path is
        // a no-op and the supervisor stays on Python -- the production
        // default until PR 7 ships.
        let delegate_requested = worker_rust::should_delegate_to_worker_rust();
        let mut swap_succeeded = true;
        if delegate_requested {
            if let Err(err) = worker_rust::swap_command_to_worker_rust(&mut effective_command) {
                let _ = self.tx.send(RuntimeEvent::Stderr(format!(
                    "[runtime] worker-rust delegation failed ({err}); \
                     falling back to the Python orchestrator"
                )));
                swap_succeeded = false;
            }
        }
        // Claude review comment #3523185636 on PR #434: fold the
        // delegate decision AND the audio-bridge decision through
        // one pure helper so a swap failure resets BOTH flags
        // together -- an earlier iteration only reset the delegate
        // flag and left `use_rust_audio` stale, which meant the
        // fallback Python child spawned without
        // `--audio-source=rust-stdin` while the audio bridge still
        // wrote JSON frames into its unread pipe.
        let plan = worker_rust::plan_worker_rust_delegation(
            delegate_requested,
            swap_succeeded,
            use_rust_audio,
        );
        let delegate_to_worker_rust = plan.delegate;
        let mut use_rust_audio = use_rust_audio;
        if plan.delegate {
            use_rust_audio = false;
        }
        if plan.push_rust_stdin_arg {
            effective_command
                .args
                .push("--audio-source=rust-stdin".to_owned());
        }

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
        if delegate_to_worker_rust {
            // The worker-rust subprocess owns its own hotkey install +
            // session sink in-process; the supervisor MUST NOT install
            // a second listener (it would race the subprocess for the
            // same OS chord) and MUST NOT register a session sink (the
            // subprocess holds the only `DictateSession` instance).
            // Codex P2 #416 runtime.rs:1427 still applies for the
            // non-delegated rust-session path; this branch short-
            // circuits it because Python is not being spawned at all.
        } else if self.hotkey_handle.is_none() {
            if let Some(handle) = install_rust_hotkey_from_command(
                &effective_command,
                self.tx.clone(),
                self.repaint_notifier.clone(),
            ) {
                if rust_session_sink::dictate_backend_rust_session_requested() {
                    disable_python_hotkey(&mut effective_command);
                }
                self.hotkey_handle = Some(handle);
            }
        } else if let Some(handle) = self.hotkey_handle.as_ref() {
            // Restart path: the handle survived from a prior start(); the
            // env-var-derived backend choice is fixed per-process so we
            // re-evaluate whether to park Python every time.
            //
            // Codex P2 #416 (round 2) runtime.rs:511 -- validate the
            // (possibly updated) key binding against the Rust backend's
            // rdev-supported list BEFORE parking Python. Without this, a
            // Settings change to a key that the Python evdev backend
            // accepts but rdev does not (eg `super_l`) would leave Python
            // disabled and the Rust hotkey unable to fire -- PTT goes
            // silent for the whole session.
            //
            // `resume()` itself only calls `manager.register()` and does
            // not re-run install-time validation, so this gate is the
            // only place restart-time key changes are sanity-checked.
            //
            // Fix 3 (#373): Resume the manager with the (possibly new) PTT
            // key names so a changed binding takes effect without
            // restarting the whole process. The coordinator mode
            // (hold-to-talk vs. toggle) is fixed at install time; mode
            // changes require an app restart.
            //
            // The branch decision is extracted to `restart_hotkey_decision`
            // so the three-way gate (no-key / unsupported / resume) is
            // unit-testable without populating a live HotkeyHandle (Codex
            // P2 PR #421 runtime.rs:530).
            match restart_hotkey_decision(
                &effective_command,
                rust_session_sink::dictate_backend_rust_session_requested(),
            ) {
                RestartHotkeyDecision::SkipNoKey => {}
                RestartHotkeyDecision::SkipUnsupported { key_names } => {
                    eprintln!(
                        "[hotkey] resume skipped: PTT keys {key_names:?} are not \
                         supported by the Rust backend; keeping Python listener \
                         engaged and skipping resume"
                    );
                }
                RestartHotkeyDecision::Resume {
                    key_names,
                    park_python,
                } => {
                    if park_python {
                        disable_python_hotkey(&mut effective_command);
                    }
                    handle.resume(key_names);
                }
            }
        }

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
        if use_rust_audio {
            // Only the Rust path needs to write to the worker's stdin
            // (JSON frame events). Inherit otherwise — the Python path
            // doesn't read stdin and a piped+unused stdin can confuse
            // some libraries that probe `isatty()` on launch.
            process.stdin(Stdio::piped());
        }
        configure_piped_python_stdio(&mut process);
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

        // Fix 4 (#373): suspend Rust hotkey tracking while the worker is
        // down so PTT presses during a stopped period don't leave the
        // coordinator in Recording state at the next start(). The manager
        // unregisters its binding so no tracker outputs flow; Cancel resets
        // Recording → Idle. A coordinator stuck in Processing (transcription
        // was in-flight at stop) remains there until the next
        // ProcessingFinished — that is acceptable because Python handles
        // actual recording so correctness is unaffected.
        if let Some(handle) = self.hotkey_handle.as_ref() {
            handle.suspend();
        }

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

    /// When the rust-session sink is active, suspend the hotkey
    /// handle on an unexpected child exit so PTT presses do not keep
    /// driving the in-process [`crate::dictate::DictateSession`] while
    /// the UI considers the runtime stopped. Codex P2 #416
    /// runtime.rs:1484.
    ///
    /// No-op for the logger-sink path: the logger sink is harmless
    /// (just stderr lines), and Python -- which owned the recording
    /// lifecycle in that path -- already exited together with the
    /// child. Leaving the Rust manager registered there preserves the
    /// existing PR 1-3 behaviour exactly.
    ///
    /// The next `start()` call's restart-path branch then re-registers
    /// the binding via `handle.resume(key_names)` so PTT comes back
    /// online with the (possibly updated) chord.
    fn suspend_session_sink_on_exit(&self) {
        if !rust_session_sink::dictate_backend_rust_session_requested() {
            return;
        }
        if let Some(handle) = self.hotkey_handle.as_ref() {
            handle.suspend();
        }
    }

    /// Mirror of [`Self::suspend_session_sink_on_exit`] for the
    /// `start()` error-return paths. Codex P2 #416 (round 2)
    /// runtime.rs:504: the Rust hotkey handle is installed (and the
    /// session sink registered with the coordinator) BEFORE the
    /// fallible `process.spawn()` + audio-bridge prep steps. If those
    /// fail and `start()` returns Err, the UI flips to Stopped but
    /// the hotkey handle is still live -- PTT presses can still drive
    /// `DictateSession` against a worker that never started.
    ///
    /// Suspend the handle on those paths so the coordinator returns
    /// to Idle and PTT goes silent until the next successful start.
    /// Same gate as the on-exit path: only the session-sink build
    /// needs cleanup (the logger sink is inert).
    fn suspend_session_sink_on_start_failure(&self) {
        self.suspend_session_sink_on_exit();
    }

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
    pub fn dispatch_external_command(&self, cmd: external_toggle::ExternalCommand) {
        // Fast path: surface the trigger on the runtime event log so the UI
        // shows external-toggle activity even when there's no coordinator
        // to route it through.
        let _ = self.tx.send(RuntimeEvent::Stderr(format!(
            "[external-toggle] received {cmd:?}"
        )));
        #[cfg(feature = "rust-hotkeys")]
        {
            use crate::hotkey::coordinator::CoordinatorEvent;
            use external_toggle::ExternalCommand;
            if let Some(handle) = self.hotkey_handle.as_ref() {
                let event = match cmd {
                    ExternalCommand::Toggle => CoordinatorEvent::ExternalToggle,
                    // Claude P1 #428: Start/Stop must NOT collapse to
                    // ExternalToggle -- that gives the CLI flags opposite
                    // semantics (--start-recording ends up stopping when
                    // already recording, and vice versa). Route each to
                    // its dedicated idempotent coordinator event.
                    ExternalCommand::Start => CoordinatorEvent::ExternalStart,
                    ExternalCommand::Stop => CoordinatorEvent::ExternalStop,
                    ExternalCommand::Cancel => CoordinatorEvent::Cancel,
                };
                handle.send_event(event);
                return;
            }
        }
        // Either the binary was built without `rust-hotkeys`, or no
        // coordinator has been installed for this run (Python backend).
        // Surface a one-line stderr hint so users wiring the signal know
        // why nothing happened. We intentionally don't drive the Python
        // worker directly here — that needs a richer IPC and is tracked
        // in the follow-up bullet on issue #326.
        let _ = self.tx.send(RuntimeEvent::Stderr(
            "[external-toggle] no Rust hotkey coordinator active; \
             external triggers require VOICEPI_HOTKEY_BACKEND=rust on a \
             binary built with --features rust-hotkeys"
                .to_owned(),
        ));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Windows,
    Unix,
}

impl Platform {
    fn current() -> Self {
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
    fn display(&self) -> String {
        let mut parts = vec![self.program.display().to_string()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallPlan {
    app_root: PathBuf,
    requirements: PathBuf,
    venv_python: PathBuf,
    create_venv: Option<PlannedCommand>,
    install_commands: Vec<PlannedCommand>,
}

impl InstallPlan {
    fn for_current_environment(app_root: PathBuf) -> Result<Self> {
        let requirements = requirements_path(&app_root)?;
        let platform = Platform::current();
        let bootstrap_python = env::var_os(BOOTSTRAP_PYTHON_ENV)
            .or_else(|| env::var_os(PYTHON_ENV))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(default_python_name()));

        if let Some(override_python) = env::var_os(PYTHON_ENV) {
            let mut plan =
                Self::from_parts(app_root, requirements, PathBuf::from(override_python), None);
            plan.add_optional_requirements();
            return Ok(plan);
        }

        let home = home_dir().ok_or_else(|| anyhow!("HOME/USERPROFILE is not set"))?;
        let venv_dir = default_venv_dir(&home, platform);
        let venv_python = venv_python_path(&venv_dir, platform);
        let create_venv = (!venv_python.exists()).then(|| PlannedCommand {
            program: bootstrap_python,
            args: vec![
                "-m".to_owned(),
                "venv".to_owned(),
                venv_dir.display().to_string(),
            ],
            working_dir: app_root.clone(),
        });

        let mut plan = Self::from_parts(app_root, requirements, venv_python, create_venv);
        plan.add_optional_requirements();
        Ok(plan)
    }

    fn from_parts(
        app_root: PathBuf,
        requirements: PathBuf,
        venv_python: PathBuf,
        create_venv: Option<PlannedCommand>,
    ) -> Self {
        let install_commands = vec![
            PlannedCommand {
                program: venv_python.clone(),
                args: vec![
                    "-m".to_owned(),
                    "pip".to_owned(),
                    "install".to_owned(),
                    "--upgrade".to_owned(),
                    "pip".to_owned(),
                ],
                working_dir: app_root.clone(),
            },
            pip_install_command(&venv_python, &requirements, &app_root),
        ];

        Self {
            app_root,
            requirements,
            venv_python,
            create_venv,
            install_commands,
        }
    }

    fn add_optional_requirements(&mut self) {
        if wants_cuda_runtime() {
            if let Some(requirements) = first_existing_requirements(
                &self.app_root,
                &["requirements/gpu.txt", "requirements-gpu.txt"],
            ) {
                self.install_commands.push(pip_install_command(
                    &self.venv_python,
                    &requirements,
                    &self.app_root,
                ));
            }
        }
        // Wave 8 of #348 removed the optional Parakeet/NeMo install
        // step here together with the backend itself; the only optional
        // requirements file the installer still appends is the CUDA
        // bundle gated above on `wants_cuda_runtime()`.
    }

    fn run(&self) -> Result<()> {
        println!(
            "Installing whisper-dictate runtime with {}",
            self.venv_python.display()
        );
        println!("Requirements: {}", self.requirements.display());
        if let Some(command) = &self.create_venv {
            run_install_command(command)?;
        }
        for command in &self.install_commands {
            run_install_command(command)?;
        }
        println!("Install complete. Run `whisper-dictate doctor` to verify the runtime.");
        Ok(())
    }
}

fn pip_install_command(venv_python: &Path, requirements: &Path, app_root: &Path) -> PlannedCommand {
    PlannedCommand {
        program: venv_python.to_path_buf(),
        args: vec![
            "-m".to_owned(),
            "pip".to_owned(),
            "install".to_owned(),
            "-r".to_owned(),
            requirements.display().to_string(),
        ],
        working_dir: app_root.to_path_buf(),
    }
}

fn run_install_command(command: &PlannedCommand) -> Result<()> {
    println!("> {}", command.display());
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .current_dir(&command.working_dir);
    configure_background_process(&mut process);
    let status = process.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("install command failed with status {status}"))
    }
}

fn wants_cuda_runtime() -> bool {
    if env::var("VOICEPI_DEVICE").is_ok_and(|value| value.eq_ignore_ascii_case("cuda")) {
        return true;
    }
    config::load_settings()
        .map(|settings| settings.device.eq_ignore_ascii_case("cuda"))
        .unwrap_or(false)
}

fn requirements_path(app_root: &Path) -> Result<PathBuf> {
    if let Some(path) = first_existing_requirements(
        app_root,
        &[
            "requirements/cpu.txt",
            "requirements/gpu.txt",
            "requirements-cpu.txt",
            "requirements-gpu.txt",
            "requirements.txt",
        ],
    ) {
        return Ok(path);
    }
    Err(anyhow!(
        "no requirements file found in {}",
        app_root.display()
    ))
}

fn first_existing_requirements(app_root: &Path, candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(|candidate| {
            candidate
                .split('/')
                .fold(app_root.to_path_buf(), |path, part| path.join(part))
        })
        .find(|path| path.exists())
}

fn python_source_root(app_root: &Path) -> PathBuf {
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
    if let Ok(exe) = env::current_exe() {
        env.push((RUST_INJECTOR_ENV.to_owned(), exe.display().to_string()));
    }
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

/// Normalise rdev-specific PTT-key aliases in the worker command's
/// `VOICEPI_KEY` to their canonical pynput-compatible names so the Python
/// listener doesn't terminate the worker at startup when it tries to
/// resolve them against `pynput.keyboard.Key.<name>`.
///
/// Today this maps `right_alt` and `ralt` → `alt_gr` (the canonical
/// right-Alt name pynput knows). The Rust hotkey backend accepts both
/// aliases for user-facing convenience (P2 #346 finding 4); without this
/// post-processing a user that configured `right_alt` would crash the
/// Python worker even when the Rust listener took over the hotkey
/// lifecycle, because pynput resolves the keyname at startup BEFORE the
/// listener registers (so `VOICEPI_PYTHON_HOTKEY=0` does not save us).
///
/// Applied to every `+`-separated segment so chord bindings like
/// `ctrl_r+right_alt` work too. P3 #383.
pub(crate) fn normalise_hotkey_aliases_for_python(env: &mut [(String, String)]) {
    for (key, value) in env.iter_mut() {
        if key == "VOICEPI_KEY" {
            *value = normalise_hotkey_chord_for_python(value);
        }
    }
}

/// Pure helper for [`normalise_hotkey_aliases_for_python`] — split out so
/// the alias transformation can be unit-tested without constructing a
/// full WorkerCommand env vector. Preserves the input's `+` separators
/// and per-segment formatting; only the matched aliases are rewritten.
pub(crate) fn normalise_hotkey_chord_for_python(raw: &str) -> String {
    raw.split('+')
        .map(
            |segment| match segment.trim().to_ascii_lowercase().as_str() {
                "right_alt" | "ralt" => "alt_gr".to_owned(),
                _ => segment.to_owned(),
            },
        )
        .collect::<Vec<_>>()
        .join("+")
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

/// Set the `VOICEPI_PYTHON_HOTKEY=0` env var on `command` so the spawned
/// Python worker's `KeyBackendMixin.run` parks itself instead of installing
/// pynput/evdev. The supervisor MUST only call this after it has
/// successfully installed the Rust hotkey subsystem
/// ([`maybe_install_rust_hotkey`] returned `Ok`) — otherwise neither
/// backend will be listening and PTT will be broken for the entire session.
///
/// Idempotent: if the flag is already present in `command.env`, the value
/// is replaced.
///
/// **Known limitation (PR #344 P2 #7).** The Rust backend only owns the
/// PTT chord; the user-configurable multi-press quit key
/// (`VOICEPI_QUIT_KEY` / `VOICEPI_QUIT_COUNT`, default 3× Esc) lives in
/// the Python listener that this flag parks. When the Rust backend is
/// active, users can still quit via Ctrl+C from the terminal and via the
/// UI tray menu, but the configured multi-press hotkey is inactive. A
/// follow-up will carry the quit binding into the Rust path; the warning
/// emitted at install time documents the current state.
pub fn disable_python_hotkey(command: &mut WorkerCommand) {
    const KEY: &str = "VOICEPI_PYTHON_HOTKEY";
    if let Some(slot) = command.env.iter_mut().find(|(k, _)| k == KEY) {
        slot.1 = "0".to_owned();
        return;
    }
    command.env.push((KEY.to_owned(), "0".to_owned()));
}

/// Outcome of the restart-path key-binding decision (see
/// [`restart_hotkey_decision`]). Mapped 1:1 onto the supervisor's
/// restart branch in [`RuntimeSupervisor::start`] (`else if let
/// Some(handle) = self.hotkey_handle.as_ref()`):
///
/// * [`SkipNoKey`](Self::SkipNoKey) — `VOICEPI_KEY` is unset or blank;
///   nothing to resume. The previous successful install handled the
///   Python-park gate; this branch must leave it alone.
/// * [`SkipUnsupported`](Self::SkipUnsupported) — the configured key
///   is not in the Rust (rdev) supported list. The supervisor MUST
///   NOT park Python on this branch — otherwise PTT goes silent for
///   the whole session, the regression Codex P2 PR #421
///   runtime.rs:530 guards against.
/// * [`Resume`](Self::Resume) — the key is supported; the supervisor
///   calls `handle.resume(key_names)`. `park_python` is true only
///   when [`rust_session_sink::dictate_backend_rust_session_requested`]
///   is true (the in-process session sink owns the lifecycle).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RestartHotkeyDecision {
    SkipNoKey,
    SkipUnsupported {
        key_names: Vec<String>,
    },
    Resume {
        key_names: Vec<String>,
        park_python: bool,
    },
}

/// Pure decision helper for the supervisor's restart-path branch.
/// Extracted so the gate is unit-testable WITHOUT populating a live
/// [`crate::hotkey::HotkeyHandle`] (which requires the `rust-hotkeys`
/// feature plus a working rdev install — neither available in
/// headless CI).
///
/// Codex P2 PR #421 runtime.rs:530 — the matching unit test in
/// `hotkey_supervisor_tests` asserts the three branches of this
/// helper so a future edit that flipped the gate (e.g. parking
/// Python BEFORE validating) would have to fail a test before it
/// could land.
pub(crate) fn restart_hotkey_decision(
    command: &WorkerCommand,
    rust_session_requested: bool,
) -> RestartHotkeyDecision {
    let key_names = extract_hotkey_key_names(command);
    if key_names.is_empty() {
        return RestartHotkeyDecision::SkipNoKey;
    }
    if crate::hotkey::validate_key_names(&key_names).is_err() {
        return RestartHotkeyDecision::SkipUnsupported { key_names };
    }
    RestartHotkeyDecision::Resume {
        key_names,
        park_python: rust_session_requested,
    }
}

/// Whether the user requested the Rust-side hotkey backend via
/// `VOICEPI_HOTKEY_BACKEND=rust`. Pure helper so the gate is unit-testable.
/// Delegates to [`crate::hotkey::rust_hotkey_backend_requested`].
pub fn rust_hotkey_backend_requested() -> bool {
    crate::hotkey::rust_hotkey_backend_requested()
}

/// True when the user requested the Rust hotkey backend AND the binary was
/// built with the `rust-hotkeys` feature. The supervisor uses this to decide
/// whether to install the Rust hotkey subsystem and silence the Python
/// listener; both must be true, otherwise we stay on pynput (and log a
/// one-line warning if only the env var is set — handled at install time).
pub fn rust_hotkey_backend_active() -> bool {
    crate::hotkey::rust_hotkey_backend_requested() && crate::hotkey::rust_hotkey_backend_available()
}

/// Install the Rust hotkey subsystem at supervisor startup, if requested.
///
/// Returns `Some(handle)` ONLY when the env var was set, the feature is
/// compiled in, AND every layer (target validation, listener startup,
/// register) succeeded. Returns `None` (and logs a one-line warning when
/// there's something to warn about) in every other case.
///
/// **Python is NOT disabled** even on a successful install. The coordinator
/// today only logs coordinator actions ([`crate::hotkey::coordinator::CoordinatorAction`]
/// as `[hotkey]` lines); no IPC yet drives the worker's actual recording
/// lifecycle. Disabling Python before that IPC is wired would leave PTT
/// completely silent. Follow-up work will wire the IPC and then add the
/// disable step. Until then both backends run side-by-side (PR #373 Fix 1).
///
/// `action_sink` is invoked on the coordinator thread for every
/// [`crate::hotkey::coordinator::CoordinatorAction`] the state machine
/// emits; the caller is responsible for translating those into worker
/// start / stop commands (today: logging only — see above).
///
/// The caller MUST send
/// [`crate::hotkey::coordinator::CoordinatorEvent::ProcessingFinished`]
/// (with the matching recording id) via
/// [`crate::hotkey::HotkeyHandle::processing_finished`] when the worker
/// finishes transcription — otherwise the coordinator stays parked in
/// [`crate::hotkey::coordinator::Stage::Processing`] and ignores the next
/// press.
pub fn maybe_install_rust_hotkey<F>(
    key_names: Vec<String>,
    mode: crate::hotkey::coordinator::Mode,
    action_sink: F,
) -> Option<crate::hotkey::HotkeyHandle>
where
    F: FnMut(crate::hotkey::coordinator::CoordinatorAction) + Send + 'static,
{
    if !crate::hotkey::rust_hotkey_backend_requested() {
        return None;
    }
    if !crate::hotkey::rust_hotkey_backend_available() {
        eprintln!(
            "[hotkey] VOICEPI_HOTKEY_BACKEND=rust was set but this binary was \
             built without --features rust-hotkeys; falling back to the Python \
             listener (pynput). Rebuild with the feature to use the Rust backend."
        );
        return None;
    }
    let cfg = crate::hotkey::HotkeyConfig { key_names, mode };
    match crate::hotkey::install_hotkey(cfg, action_sink) {
        Ok(handle) => {
            // P2 #7: the Rust backend only owns the PTT chord; the
            // configured multi-press quit key (VOICEPI_QUIT_KEY /
            // VOICEPI_QUIT_COUNT, default 3× Esc) lives in the Python
            // listener that the supervisor is about to park. Warn so
            // users who rely on the hotkey quit know it's inactive.
            eprintln!(
                "[hotkey] Rust backend active; the configured multi-press \
                 quit key (VOICEPI_QUIT_KEY / VOICEPI_QUIT_COUNT) is \
                 currently only honoured by the Python listener. Quit \
                 via Ctrl+C in the terminal or the tray menu."
            );
            Some(handle)
        }
        Err(err) => {
            eprintln!(
                "[hotkey] Rust hotkey backend install failed: {err}; \
                 falling back to the Python listener (pynput)."
            );
            None
        }
    }
}

/// Extract the PTT key names and toggle mode from a [`WorkerCommand`]'s
/// environment, then call [`maybe_install_rust_hotkey`] with one of two
/// action sinks:
///
/// * **Default (production today):** a logger sink that turns each
///   [`crate::hotkey::coordinator::CoordinatorAction`] into a
///   `[hotkey]`-prefixed `RuntimeEvent::Stderr` line. The Python worker
///   still owns the live recording lifecycle.
/// * **`VOICEPI_DICTATE_BACKEND=rust-session`** (Wave 5 PR 4 of #348):
///   a session-backed sink that drives an in-process
///   [`crate::dictate::DictateSession`] (start / stop_and_transcribe /
///   cancel) and signals
///   [`crate::hotkey::coordinator::CoordinatorEvent::ProcessingFinished`]
///   back into the coordinator when the stop completes. Uses stub
///   `TranscribeBackend` / `InjectBackend` impls (PR 5 swaps them for
///   `LocalWhisper` + the real injection dispatcher). Frames stay
///   unwired until PR 3 (#415) + PR 5 land; the env-var gate keeps
///   production on the logger sink so PR 4 is observable but inert.
///
/// Returns the live handle on success so the caller can call
/// [`disable_python_hotkey`]; returns `None` when the backend is not
/// requested, not available, or the key config is missing / empty.
///
/// P2 #346 finding 1 (logger sink) + Wave 5 PR 4 (session sink).
fn install_rust_hotkey_from_command(
    command: &WorkerCommand,
    tx: std::sync::mpsc::Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) -> Option<crate::hotkey::HotkeyHandle> {
    let key_names = extract_hotkey_key_names(command);
    if key_names.is_empty() {
        return None;
    }
    // P2 #373 finding 2: accept all truthy values (`true`, `1`, `yes`, `on`,
    // case-insensitive) instead of only `"True"` and `"1"`.
    let toggle = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_TOGGLE")
        .map(|(_, v)| parse_toggle_value(v))
        .unwrap_or(false);
    let mode = if toggle {
        crate::hotkey::coordinator::Mode::Toggle
    } else {
        crate::hotkey::coordinator::Mode::HoldToTalk
    };
    if rust_session_sink::dictate_backend_rust_session_requested() {
        install_session_sink_hotkey(key_names, mode, tx, repaint_notifier)
    } else {
        // Logger sink does not need the notifier -- the existing
        // stream_lines path already invokes the repaint_notifier after
        // every event it enqueues, and the logger sink's outputs flow
        // through the same channel without the in-process bypass that
        // the session sink uses.
        let _ = repaint_notifier;
        install_logger_sink_hotkey(key_names, mode, tx)
    }
}

/// The historical (PR 1-3) logger sink: turn every
/// [`crate::hotkey::coordinator::CoordinatorAction`] into a stderr line
/// on the runtime event channel. Production still uses this -- the
/// Python worker owns the recording lifecycle until PR 6.
fn install_logger_sink_hotkey(
    key_names: Vec<String>,
    mode: crate::hotkey::coordinator::Mode,
    tx: std::sync::mpsc::Sender<RuntimeEvent>,
) -> Option<crate::hotkey::HotkeyHandle> {
    maybe_install_rust_hotkey(key_names, mode, move |action| {
        use crate::hotkey::coordinator::CoordinatorAction;
        let msg = match action {
            CoordinatorAction::StartRecording(id) => {
                format!("[hotkey] start_recording id={id}")
            }
            CoordinatorAction::StopAndTranscribe(id) => {
                format!("[hotkey] stop_and_transcribe id={id}")
            }
            CoordinatorAction::CancelRecording(id) => {
                format!("[hotkey] cancel_recording id={id}")
            }
        };
        let _ = tx.send(RuntimeEvent::Stderr(msg));
    })
}

/// Wave 5 PR 4 of #348: opt-in session-backed sink.
///
/// Wires the coordinator into a [`crate::dictate::DictateSession`] so
/// PTT press/release actually drives `session.start()` /
/// `stop_and_transcribe()` / `cancel(epoch)`. The session feeds
/// worker events back through `tx` as
/// [`RuntimeEvent::Worker`] just like the Python worker does, so the
/// UI's log card / tray-state machine see the same shape.
///
/// After `install_hotkey` succeeds, the
/// [`crate::hotkey::HotkeyHandle::coordinator_handle`] is poured into
/// the shared `OnceLock` the sink captured, so the sink's
/// `processing_finished` callback can send
/// [`crate::hotkey::coordinator::CoordinatorEvent::ProcessingFinished`]
/// back into the coordinator (the press latch otherwise stays parked
/// in `Stage::Processing`). On stub builds (no `rust-hotkeys`
/// feature) `maybe_install_rust_hotkey` returns `None` before we ever
/// touch the slot, so the cfg-gated accessor is only invoked on
/// builds where it exists.
fn install_session_sink_hotkey(
    key_names: Vec<String>,
    mode: crate::hotkey::coordinator::Mode,
    tx: std::sync::mpsc::Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) -> Option<crate::hotkey::HotkeyHandle> {
    let (sink, coord_slot) = rust_session_sink::build_production_sink(tx.clone(), repaint_notifier);
    let handle = maybe_install_rust_hotkey(key_names, mode, sink)?;
    #[cfg(feature = "rust-hotkeys")]
    {
        // `set` returns Err iff the slot is already populated. This is
        // the only writer; a duplicate `Some(handle)` from a future
        // refactor would be a programming error worth logging but not
        // panicking on -- the existing handle would still drive
        // ProcessingFinished correctly.
        if coord_slot.set(handle.coordinator_handle()).is_err() {
            let _ = tx.send(RuntimeEvent::Stderr(
                "[rust-session] coordinator handle slot already populated; \
                 ignoring (this is a no-op but indicates a refactor regression)"
                    .to_owned(),
            ));
        }
    }
    // Suppress `unused` warnings when `rust-hotkeys` is off: the slot
    // would never be populated, but `build_production_sink` already
    // returned it; explicitly discard so clippy stays quiet.
    #[cfg(not(feature = "rust-hotkeys"))]
    let _ = coord_slot;
    Some(handle)
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
fn propagate_rust_devices_backend(command: &mut WorkerCommand) {
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
    install_command_from_exe(
        env::current_exe().unwrap_or_else(|_| PathBuf::from("whisper-dictate")),
        app_root(),
    )
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

#[derive(Debug)]
pub struct WorkerOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: ExitStatus,
}

impl WorkerOutput {
    pub fn success(&self) -> bool {
        self.status.success()
    }

    pub fn code(&self) -> Option<i32> {
        self.status.code()
    }
}

pub fn run_capture(command: &WorkerCommand) -> Result<WorkerOutput> {
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .current_dir(&command.working_dir)
        .envs(command.env.iter().map(|(key, value)| (key, value)));
    configure_piped_python_stdio(&mut process);
    configure_background_process(&mut process);
    let output = process.output()?;

    Ok(WorkerOutput {
        stdout: decode_capped_output(&output.stdout),
        stderr: decode_capped_output(&output.stderr),
        status: output.status,
    })
}

pub fn decode_capped_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.as_ref();
    if text.len() <= CAPTURE_OUTPUT_MAX_CHARS {
        return text.to_owned();
    }
    let marker = "[ui] ...older captured output trimmed...\n";
    let target = CAPTURE_OUTPUT_MAX_CHARS.saturating_sub(marker.len());
    let mut start = text.len().saturating_sub(target);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!("{marker}{}", &text[start..])
}

pub fn run_foreground(command: &WorkerCommand) -> Result<()> {
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .current_dir(&command.working_dir)
        .envs(command.env.iter().map(|(key, value)| (key, value)));
    // Force the worker's stdio to UTF-8 so foreground commands like
    // `whisper-dictate bench > out.txt` or `whisper-dictate corpus-record <id>`
    // do not mojibake / EncodingError on Windows when the inherited console
    // code page is non-UTF-8 (cp1252 / cp437 / Shift-JIS …). The Python
    // worker writes `ensure_ascii=False` JSONL with Danish corpus text and
    // user dictionary terms; matches the `configure_piped_python_stdio` the
    // captured/background paths already set, so all foreground workers see
    // the same encoding regardless of how the user shells in.
    configure_piped_python_stdio(&mut process);
    let status = process.status()?;
    exit_status_to_result(status)
}

fn configure_background_process(_command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        _command.creation_flags(CREATE_NO_WINDOW);
    }
}

fn configure_piped_python_stdio(command: &mut Command) {
    command
        .env(PYTHON_UTF8_ENV, "1")
        .env(PYTHON_IO_ENCODING_ENV, "utf-8");
}

fn exit_status_to_result(status: ExitStatus) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("worker exited with status {status}"))
    }
}

#[derive(Debug, Clone, Copy)]
enum RuntimeStream {
    Stdout,
    Stderr,
}

fn stream_lines<R>(
    reader: R,
    tx: Sender<RuntimeEvent>,
    stream: RuntimeStream,
    repaint_notifier: Option<RepaintNotifier>,
    ready_signal: Option<Sender<()>>,
    /// Codex P2 (runtime.rs:2074, PR #440) — observer generation
    /// captured when this reader is created. `None` on streams that
    /// never emit worker events (e.g. stdout). See
    /// [`crate::output_mute::session::current_generation`].
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
                        // Issue #324: persist one row per accepted
                        // utterance into the per-user SQLite history
                        // store. Best-effort and feature-gated — the
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
    let config_value = config::load_raw_config()
        .ok()
        .and_then(|value| {
            value
                .as_object()
                .and_then(read_mute_key_option)
        });
    crate::output_mute::session::install_from_settings(config_value);
}

/// Read the raw `mute_output_while_recording` value from a config map.
///
/// Returns `None` when the key is absent (so the session installer can
/// fall back to the env var / default). Returns `Some(true|false)` when
/// the key is present, using the same lax-bool vocabulary as
/// `config::load::bool_value`. An unparseable value (e.g. `"maybe"`)
/// also returns `None` so the env fallback wins.
fn read_mute_key_option(object: &serde_json::Map<String, serde_json::Value>) -> Option<bool> {
    let value = object.get("mute_output_while_recording")?;
    match value {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "" | "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
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

fn python_program() -> PathBuf {
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

fn default_venv_dir(home: &Path, platform: Platform) -> PathBuf {
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
fn windows_venv_dir(home: &Path) -> PathBuf {
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

fn venv_python_path(venv_dir: &Path, platform: Platform) -> PathBuf {
    match platform {
        Platform::Windows => venv_dir.join("Scripts").join("python.exe"),
        Platform::Unix => venv_dir.join("bin").join("python"),
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn default_python_name() -> &'static str {
    if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    }
}

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

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

fn app_root_from_exe_path(exe: &Path) -> Option<PathBuf> {
    let root = exe.parent()?;
    python_source_root(root)
        .join("whisper_dictate")
        .join("runtime.py")
        .exists()
        .then(|| root.to_path_buf())
}

pub mod audio_spawn;
// Issue #327: cross-platform single-instance gate with CLI-arg
// forwarding. Split into `single_instance/{mod,socket,lockfile}.rs`
// so each file stays under the project's 500-LOC modularity ceiling.
// The module ships the machinery; opt-in wire-up lives in
// `main.rs::run()` behind `VOICEPI_SINGLE_INSTANCE`.
pub mod single_instance;
// Wave 5 PR 6 of #348: the `whisper-dictate worker-rust` CLI entry
// point + supervisor-side "delegate to worker-rust subprocess" gate.
// Production code path (without VOICEPI_DICTATE_BACKEND=rust-session +
// all four features) is byte-for-byte unchanged: when the env var is
// unset OR the feature set is incomplete, the supervisor stays on
// Python. PR 7 will flip the default and delete the Python orchestrator.
pub mod worker_rust;
// Issue #326: CLI flags + Unix signals (SIGUSR1 = toggle, SIGUSR2 = cancel)
// for compositor-driven external triggers. Lives in its own file so
// `runtime.rs` does not grow past the 500-LOC modularity guideline. See
// the module docs for the wire protocol and the daemon-side install hook.
pub mod external_toggle;
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
#[cfg(test)]
mod desktop_entry_tests;
// Issue #326: sibling tests for `external_toggle`.
#[cfg(test)]
mod external_toggle_tests;
#[cfg(test)]
mod hotkey_supervisor_tests;
#[cfg(test)]
mod install_plan_tests;
#[cfg(test)]
mod process_capture_tests;
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
mod worker_command_tests;
#[cfg(test)]
mod worker_event_tests;
