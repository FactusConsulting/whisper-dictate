use std::env;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{anyhow, Result};
use serde_json::Value;

const PYTHON_ENV: &str = "VOICEPI_PYTHON";
const APP_ROOT_ENV: &str = "VOICEPI_APP_ROOT";
const WORKER_EVENTS_ENV: &str = "VOICEPI_WORKER_EVENTS";
const WORKER_EVENT_PREFIX: &str = "[worker-event] ";

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

pub fn run_terminal() -> Result<()> {
    let command = default_worker_command();
    run_foreground(&command)
}

pub fn doctor() -> Result<()> {
    println!("Rust doctor command is scaffolded. Dependency checks will move here from the platform scripts.");
    Ok(())
}

pub fn install() -> Result<()> {
    println!("Rust install command is scaffolded. Bootstrap work is tracked in issue #29.");
    Ok(())
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
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .current_dir(&command.working_dir)
            .env(WORKER_EVENTS_ENV, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

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

        kill_child(&mut child)?;
        let status = child.wait()?;
        self.state = RuntimeState::Stopped;
        let _ = self.tx.send(RuntimeEvent::Exited {
            code: status.code(),
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
}

impl WorkerCommand {
    pub fn display(&self) -> String {
        let mut parts = vec![self.program.display().to_string()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

pub fn worker_command(app_root: impl AsRef<Path>) -> WorkerCommand {
    let app_root = app_root.as_ref().to_path_buf();
    WorkerCommand {
        program: python_program(),
        args: vec![app_root.join("voice_pi.py").display().to_string()],
        working_dir: app_root,
    }
}

pub fn default_worker_command() -> WorkerCommand {
    worker_command(app_root())
}

pub fn run_foreground(command: &WorkerCommand) -> Result<()> {
    let status = Command::new(&command.program)
        .args(&command.args)
        .current_dir(&command.working_dir)
        .status()?;
    exit_status_to_result(status)
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
    env::var_os(PYTHON_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default_python_name()))
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
    env::var_os(APP_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(source_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn runtime_state_labels_are_stable() {
        assert_eq!(RuntimeState::Stopped.label(), "Stopped");
        assert_eq!(RuntimeState::Starting.label(), "Starting");
        assert_eq!(RuntimeState::Running.label(), "Running");
    }

    #[test]
    fn worker_command_launches_python_directly() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::remove_var(PYTHON_ENV);

        let root = PathBuf::from("/tmp/whisper-dictate");
        let command = worker_command(&root);

        assert_eq!(command.program, PathBuf::from(default_python_name()));
        assert_eq!(command.args, vec!["/tmp/whisper-dictate/voice_pi.py"]);
        assert_eq!(command.working_dir, root);
    }

    #[test]
    fn worker_command_honors_python_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = PathBuf::from("/tmp/whisper-dictate");

        env::set_var(PYTHON_ENV, "/custom/python");
        let command = worker_command(root);
        env::remove_var(PYTHON_ENV);

        assert_eq!(command.program, PathBuf::from("/custom/python"));
    }

    #[test]
    fn default_worker_command_honors_app_root_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var(APP_ROOT_ENV, "/installed/app");

        let command = default_worker_command();
        env::remove_var(APP_ROOT_ENV);

        assert_eq!(command.working_dir, PathBuf::from("/installed/app"));
        assert_eq!(command.args, vec!["/installed/app/voice_pi.py"]);
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
