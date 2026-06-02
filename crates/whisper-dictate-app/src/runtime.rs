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
    let status = Command::new("bash").arg(&script).status()?;
    if status.success() {
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
        self.stop_and_wait()?;
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

    fn stop_and_wait(&mut self) -> Result<()> {
        let Some(mut child) = self.child.take() else {
            self.state = RuntimeState::Stopped;
            return Ok(());
        };

        kill_child(&mut child)?;
        let status = child.wait()?;
        self.state = RuntimeState::Stopped;
        let _ = self.tx.send(RuntimeEvent::Exited {
            code: status.code(),
        });
        Ok(())
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

fn requirements_path(app_root: &Path) -> Result<PathBuf> {
    for filename in [
        "requirements.txt",
        "requirements-cpu.txt",
        "requirements-gpu.txt",
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
        .current_dir(&command.working_dir);
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
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn voice_pi_arg(root: impl AsRef<Path>) -> String {
        root.as_ref().join("voice_pi.py").display().to_string()
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let original = env::var_os(key);
            env::set_var(key, value);
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = env::var_os(key);
            env::remove_var(key);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                env::set_var(self.key, value);
            } else {
                env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn runtime_state_labels_are_stable() {
        assert_eq!(RuntimeState::Stopped.label(), "Stopped");
        assert_eq!(RuntimeState::Starting.label(), "Starting");
        assert_eq!(RuntimeState::Running.label(), "Running");
    }

    #[test]
    fn worker_command_launches_python_directly() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
        let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");

        let root = PathBuf::from("/tmp/whisper-dictate");
        let command = worker_command(&root);

        assert_eq!(command.program, PathBuf::from(default_python_name()));
        assert_eq!(command.args, vec![voice_pi_arg("/tmp/whisper-dictate")]);
        assert_eq!(command.working_dir, root);
    }

    #[test]
    fn worker_command_appends_passthrough_args() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
        let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");

        let command = worker_command_with_args(
            "/tmp/whisper-dictate",
            ["--key".to_owned(), "shift_r+ctrl_r".to_owned()],
        );

        assert_eq!(
            command.args,
            vec![
                voice_pi_arg("/tmp/whisper-dictate"),
                "--key".to_owned(),
                "shift_r+ctrl_r".to_owned(),
            ]
        );
    }

    #[test]
    fn worker_command_honors_python_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = PathBuf::from("/tmp/whisper-dictate");

        let _python_guard = EnvVarGuard::set(PYTHON_ENV, "/custom/python");
        let command = worker_command(root);

        assert_eq!(command.program, PathBuf::from("/custom/python"));
    }

    #[test]
    fn worker_command_prefers_existing_project_venv_python() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let python = if cfg!(windows) {
            dir.path()
                .join("voice-pi-venv")
                .join("Scripts")
                .join("python.exe")
        } else {
            dir.path()
                .join(".venv-whisper-dictate")
                .join("bin")
                .join("python")
        };
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();

        let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
        let _home_guard = EnvVarGuard::set("HOME", dir.path());
        let command = worker_command("/tmp/whisper-dictate");

        assert_eq!(command.program, python);
    }

    #[test]
    fn default_worker_command_honors_app_root_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, "/installed/app");

        let command = default_worker_command();

        assert_eq!(command.working_dir, PathBuf::from("/installed/app"));
        assert_eq!(command.args, vec![voice_pi_arg("/installed/app")]);
    }

    #[test]
    fn app_root_can_be_inferred_from_installed_exe_directory() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(if cfg!(windows) {
            "whisper-dictate.exe"
        } else {
            "whisper-dictate"
        });
        std::fs::write(dir.path().join("voice_pi.py"), "").unwrap();

        assert_eq!(app_root_from_exe_path(&exe), Some(dir.path().to_path_buf()));
    }

    #[test]
    fn version_prefers_version_file_without_v_prefix() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("VERSION"), "v9.8.7\n").unwrap();
        let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, dir.path());

        assert_eq!(version(), "9.8.7");
    }

    #[cfg(windows)]
    #[test]
    fn stale_process_cleanup_script_is_scoped_to_current_exe_and_app_root() {
        let script = stale_process_cleanup_script(
            123,
            Path::new(r"C:\Program Files\WhisperDictate\whisper-dictate.exe"),
            Path::new(r"C:\Program Files\WhisperDictate"),
        );

        assert!(script.contains("$currentPid = 123"));
        assert!(script.contains("$cleanupPid = $PID"));
        assert!(script.contains(r"$_.ExecutablePath -eq $exe"));
        assert!(script.contains("$_.ProcessId -ne $cleanupPid"));
        assert!(script.contains(r#"$_.CommandLine -like "*voice_pi.py*""#));
        assert!(script.contains(r#"$_.CommandLine -like "*$root*""#));
        assert!(!script.contains("Stop-Process -Name python"));
        assert!(!script.contains("taskkill /IM python"));
    }

    #[cfg(windows)]
    #[test]
    fn powershell_single_quote_escape_doubles_quotes() {
        assert_eq!(
            escape_powershell_single_quoted(r"C:\It's\app"),
            r"C:\It''s\app"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_shell_prefers_pwsh_when_present_on_path() {
        if env::var_os("PATH")
            .map(|path| env::split_paths(&path).any(|dir| dir.join("pwsh.exe").exists()))
            .unwrap_or(false)
        {
            assert_eq!(windows_shell_program(), "pwsh.exe");
        }
    }

    #[test]
    fn doctor_command_adds_doctor_argument() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, "/installed/app");
        let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
        let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

        let command = doctor_command();

        assert_eq!(
            command.args,
            vec![voice_pi_arg("/installed/app"), "--doctor".to_owned()]
        );
    }

    #[test]
    fn install_command_runs_rust_cli_from_app_root() {
        let command = install_command_from_exe("/installed/app/whisper-dictate", "/installed/app");

        assert_eq!(
            command.program,
            PathBuf::from("/installed/app/whisper-dictate")
        );
        assert_eq!(command.args, vec!["install".to_owned()]);
        assert_eq!(command.working_dir, PathBuf::from("/installed/app"));
    }

    #[test]
    fn run_capture_returns_stdout_stderr_and_status() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let command = WorkerCommand {
            program: PathBuf::from("cmd.exe"),
            args: vec![
                "/C".to_owned(),
                "echo out line & echo err line 1>&2 & exit /B 7".to_owned(),
            ],
            working_dir: dir.path().to_path_buf(),
        };
        #[cfg(not(windows))]
        let command = WorkerCommand {
            program: PathBuf::from("sh"),
            args: vec![
                "-c".to_owned(),
                "echo out line; echo err line >&2; exit 7".to_owned(),
            ],
            working_dir: dir.path().to_path_buf(),
        };

        let output = run_capture(&command).unwrap();

        assert!(!output.success());
        assert_eq!(output.code(), Some(7));
        assert!(output.stdout.contains("out line"));
        assert!(output.stderr.contains("err line"));
    }

    #[test]
    fn install_plan_prefers_bundle_requirements_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("requirements-cpu.txt"), "").unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "").unwrap();
        let plan = InstallPlan::from_parts(
            dir.path().to_path_buf(),
            requirements_path(dir.path()).unwrap(),
            PathBuf::from("/venv/bin/python"),
            None,
        );

        assert_eq!(plan.requirements, dir.path().join("requirements.txt"));
        assert_eq!(
            plan.install_commands[1].args,
            vec![
                "-m",
                "pip",
                "install",
                "-r",
                plan.requirements.to_str().unwrap()
            ]
        );
    }

    #[test]
    fn install_plan_includes_parakeet_requirements_when_backend_requests_it() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "").unwrap();
        std::fs::write(dir.path().join("requirements-parakeet.txt"), "").unwrap();

        let _python_guard = EnvVarGuard::set(PYTHON_ENV, "/custom/python");
        let _backend_guard = EnvVarGuard::set(STT_BACKEND_ENV, "parakeet");
        let plan = InstallPlan::for_current_environment(dir.path().to_path_buf()).unwrap();

        assert_eq!(plan.install_commands.len(), 3);
        assert_eq!(
            plan.install_commands[2].args[4],
            dir.path()
                .join("requirements-parakeet.txt")
                .display()
                .to_string()
        );
    }

    #[test]
    fn install_plan_skips_missing_parakeet_requirements() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "").unwrap();

        let _python_guard = EnvVarGuard::set(PYTHON_ENV, "/custom/python");
        let _backend_guard = EnvVarGuard::set(STT_BACKEND_ENV, "parakeet");
        let plan = InstallPlan::for_current_environment(dir.path().to_path_buf()).unwrap();

        assert_eq!(plan.install_commands.len(), 2);
    }

    #[test]
    fn venv_paths_match_platform_conventions() {
        let home = PathBuf::from("/home/person");
        assert_eq!(
            venv_python_path(&default_venv_dir(&home, Platform::Unix), Platform::Unix),
            PathBuf::from("/home/person/.venv-whisper-dictate/bin/python")
        );

        let home = PathBuf::from("C:/Users/Person");
        assert_eq!(
            venv_python_path(
                &default_venv_dir(&home, Platform::Windows),
                Platform::Windows
            ),
            PathBuf::from("C:/Users/Person/voice-pi-venv/Scripts/python.exe")
        );
    }

    #[test]
    fn parses_worker_event_lines() {
        let event = parse_worker_event(
            r#"[worker-event] {"event":"status","state":"ready","model":"large-v3"}"#,
        )
        .unwrap();

        assert_eq!(event.event, "status");
        assert_eq!(event.state.as_deref(), Some("ready"));
        assert_eq!(event.payload["model"], "large-v3");
    }

    #[test]
    fn invalid_worker_event_lines_fall_back_to_stderr() {
        assert!(parse_worker_event("[worker-event] not json").is_none());
        assert!(parse_worker_event("ordinary stderr").is_none());
    }
}
