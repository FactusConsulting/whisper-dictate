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
const STT_BACKEND_ENV: &str = "VOICEPI_STT_BACKEND";
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
        if use_rust_audio {
            effective_command
                .args
                .push("--audio-source=rust-stdin".to_owned());
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
        let mut child = process.spawn()?;

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

        if let Some(stdout) = child.stdout.take() {
            stream_lines(
                stdout,
                self.tx.clone(),
                RuntimeStream::Stdout,
                self.repaint_notifier.clone(),
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

    pub fn poll(&mut self) -> Vec<RuntimeEvent> {
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
                let _ = self.tx.send(RuntimeEvent::Exited { code: exit_code });
                if let Some(notifier) = self.repaint_notifier.as_ref() {
                    notifier();
                }
            } else {
                // No child to kill (already exited): just be sure
                // state reflects stopped.
                self.state = RuntimeState::Stopped;
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
        if wants_parakeet_backend() {
            if let Some(requirements) = first_existing_requirements(
                &self.app_root,
                &["requirements/parakeet.txt", "requirements-parakeet.txt"],
            ) {
                self.install_commands.push(pip_install_command(
                    &self.venv_python,
                    &requirements,
                    &self.app_root,
                ));
            }
        }
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

fn wants_parakeet_backend() -> bool {
    if env::var(STT_BACKEND_ENV).is_ok_and(|value| value.eq_ignore_ascii_case("parakeet")) {
        return true;
    }
    config::load_settings()
        .map(|settings| settings.stt_backend.eq_ignore_ascii_case("parakeet"))
        .unwrap_or(false)
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
/// register) succeeded — the caller can then safely call
/// [`disable_python_hotkey`] on its worker command to park the Python
/// listener. Returns `None` (and logs a one-line warning when there's
/// something to warn about) in every other case: the supervisor MUST
/// leave the Python listener wired in that case, otherwise PTT is dead.
///
/// `action_sink` is invoked on the coordinator thread for every
/// [`crate::hotkey::coordinator::CoordinatorAction`] the state machine
/// emits; the caller is responsible for translating those into worker
/// start / stop commands (today: by sending a stdin line or signalling
/// the already-running worker, depending on the host).
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
pub fn audio_devices_command() -> WorkerCommand {
    default_worker_command_with_args(vec!["--list-audio-devices".to_owned()])
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
    default_worker_command_with_args(vec!["--record-corpus-item".to_owned(), id.to_owned()])
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
#[cfg(test)]
mod install_plan_tests;
#[cfg(test)]
mod process_capture_tests;
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
