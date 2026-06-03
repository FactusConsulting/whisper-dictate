use std::env;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::config;

const PYTHON_ENV: &str = "VOICEPI_PYTHON";
const BOOTSTRAP_PYTHON_ENV: &str = "VOICEPI_BOOTSTRAP_PYTHON";
const APP_ROOT_ENV: &str = "VOICEPI_APP_ROOT";
const WORKER_EVENTS_ENV: &str = "VOICEPI_WORKER_EVENTS";
const WORKER_EVENT_PREFIX: &str = "[worker-event] ";
const STT_BACKEND_ENV: &str = "VOICEPI_STT_BACKEND";
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
    let script = app_root().join("ubuntu26.04").join("setup.sh");
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

fn install_linux_desktop_entries() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    let home = home_dir().ok_or_else(|| anyhow!("HOME is not set"))?;
    let applications = home.join(".local/share/applications");
    let autostart = home.join(".config/autostart");
    std::fs::create_dir_all(&applications)?;
    std::fs::create_dir_all(&autostart)?;

    let exec = linux_desktop_exec_command();
    let desktop = linux_desktop_entry(false, &exec);
    let autostart_desktop = linux_desktop_entry(true, &exec);
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
    let icon_dir = home.join(".local/share/icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&icon_dir)?;
    std::fs::write(
        icon_dir.join("whisper-dictate.svg"),
        include_str!("../../../assets/whisper-dictate-logo.svg"),
    )?;
    Ok(())
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

fn linux_desktop_entry(autostart: bool, exec: &str) -> String {
    let mut entry = format!(
        "[Desktop Entry]\n\
Name=Whisper Dictate\n\
Comment=Push-to-talk dictation settings and runtime control\n\
Exec={exec}\n\
Icon=whisper-dictate\n\
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
      ($_.CommandLine -like "*voice_pi.py*" -and $_.CommandLine -like "*$root*")
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

#[derive(Debug)]
pub struct RuntimeSupervisor {
    child: Option<Child>,
    state: RuntimeState,
    tx: Sender<RuntimeEvent>,
    rx: Receiver<RuntimeEvent>,
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
        }
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

        self.state = RuntimeState::Starting;
        let display = command.display();
        let mut process = Command::new(&command.program);
        process
            .args(&command.args)
            .current_dir(&command.working_dir)
            .env(WORKER_EVENTS_ENV, "1")
            .envs(command.env.iter().map(|(key, value)| (key, value)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_background_process(&mut process);
        let mut child = process.spawn()?;

        if let Some(stdout) = child.stdout.take() {
            stream_lines(stdout, self.tx.clone(), RuntimeStream::Stdout);
        }
        if let Some(stderr) = child.stderr.take() {
            stream_lines(stderr, self.tx.clone(), RuntimeStream::Stderr);
        }

        self.state = RuntimeState::Running;
        self.child = Some(child);
        let _ = self.tx.send(RuntimeEvent::Started { command: display });
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        let Some(mut child) = self.child.take() else {
            self.state = RuntimeState::Stopped;
            return Ok(());
        };

        self.state = RuntimeState::Stopped;
        let tx = self.tx.clone();
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
        });
        Ok(())
    }

    pub fn restart(&mut self, command: WorkerCommand) -> Result<()> {
        self.stop()?;
        self.start(command)
    }

    pub fn poll(&mut self) -> Vec<RuntimeEvent> {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.state = RuntimeState::Stopped;
                    self.child = None;
                    let _ = self.tx.send(RuntimeEvent::Exited {
                        code: status.code(),
                    });
                }
                Ok(None) => {}
                Err(err) => {
                    self.state = RuntimeState::Stopped;
                    self.child = None;
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
            let requirements = self.app_root.join("requirements-gpu.txt");
            if requirements.exists() {
                self.install_commands.push(pip_install_command(
                    &self.venv_python,
                    &requirements,
                    &self.app_root,
                ));
            }
        }
        if wants_parakeet_backend() {
            let requirements = self.app_root.join("requirements-parakeet.txt");
            if requirements.exists() {
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
    let status = Command::new(&command.program)
        .args(&command.args)
        .current_dir(&command.working_dir)
        .status()?;
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
    for filename in [
        "requirements-cpu.txt",
        "requirements-gpu.txt",
        "requirements.txt",
    ] {
        let path = app_root.join(filename);
        if path.exists() {
            return Ok(path);
        }
    }
    Err(anyhow!(
        "no requirements file found in {}",
        app_root.display()
    ))
}

pub fn worker_command(app_root: impl AsRef<Path>) -> WorkerCommand {
    worker_command_with_args(app_root, Vec::<String>::new())
}

pub fn worker_command_with_args(
    app_root: impl AsRef<Path>,
    passthrough_args: impl IntoIterator<Item = String>,
) -> WorkerCommand {
    let app_root = app_root.as_ref().to_path_buf();
    let mut args = vec![app_root.join("voice_pi.py").display().to_string()];
    args.extend(passthrough_args);
    WorkerCommand {
        program: python_program(),
        args,
        working_dir: app_root,
        env: Vec::new(),
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
    configure_background_process(&mut process);
    let output = process.output()?;

    Ok(WorkerOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status,
    })
}

pub fn run_foreground(command: &WorkerCommand) -> Result<()> {
    let status = Command::new(&command.program)
        .args(&command.args)
        .current_dir(&command.working_dir)
        .envs(command.env.iter().map(|(key, value)| (key, value)))
        .status()?;
    exit_status_to_result(status)
}

fn configure_background_process(_command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        _command.creation_flags(CREATE_NO_WINDOW);
    }
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

fn stream_lines<R>(reader: R, tx: Sender<RuntimeEvent>, stream: RuntimeStream)
where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            match line {
                Ok(line) => {
                    let event = runtime_event_from_line(line, stream);
                    let _ = tx.send(event);
                }
                Err(err) => {
                    let _ = tx.send(RuntimeEvent::Error(err.to_string()));
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
        let status = Command::new("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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
        Platform::Windows => home.join("voice-pi-venv"),
        Platform::Unix => home.join(".venv-whisper-dictate"),
    }
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
        .nth(2)
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
    root.join("voice_pi.py")
        .exists()
        .then(|| root.to_path_buf())
}

#[cfg(test)]
mod tests;
