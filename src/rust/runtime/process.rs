//! Process-execution helpers shared by the supervisor and the
//! foreground / capture CLI paths.
//!
//! Split out of `runtime.rs` in the 500-LOC modularity refactor. Owns
//! the low-level plumbing every command constructor uses:
//! - [`configure_background_process`] / [`configure_piped_python_stdio`]
//!   — cross-platform spawn adjustments (hidden window on Windows,
//!   forced UTF-8 stdio for the Python worker),
//! - [`run_foreground`] / [`run_capture`] + [`WorkerOutput`] +
//!   [`decode_capped_output`] — synchronous command drivers,
//! - [`stream_lines`] / [`runtime_event_from_line`] /
//!   [`parse_worker_event`] — the background thread that turns child
//!   stdout/stderr into [`RuntimeEvent`]s (with the audio-in-rust ready
//!   signal handshake),
//! - [`kill_child`] — the taskkill / SIGKILL wrapper the supervisor
//!   calls on shutdown.

use std::io::{BufRead, BufReader};
#[cfg(windows)]
use std::process::Stdio;
use std::process::{Child, Command, ExitStatus};
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::supervisor::{RepaintNotifier, RuntimeEvent, WorkerEvent};
use super::worker_command::WorkerCommand;

pub(crate) const WORKER_EVENT_PREFIX: &str = "[worker-event] ";
pub(crate) const PYTHON_UTF8_ENV: &str = "PYTHONUTF8";
pub(crate) const PYTHON_IO_ENCODING_ENV: &str = "PYTHONIOENCODING";

/// Maximum captured worker output kept in memory, measured in UTF-8 bytes
/// (`str::len()`), despite the legacy `_CHARS` suffix in the public name.
pub const CAPTURE_OUTPUT_MAX_CHARS: usize = 200_000;

#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x08000000;

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

pub(crate) fn configure_background_process(_command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        _command.creation_flags(CREATE_NO_WINDOW);
    }
}

pub(crate) fn configure_piped_python_stdio(command: &mut Command) {
    command
        .env(PYTHON_UTF8_ENV, "1")
        .env(PYTHON_IO_ENCODING_ENV, "utf-8");
}

pub(crate) fn exit_status_to_result(status: ExitStatus) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("worker exited with status {status}"))
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RuntimeStream {
    Stdout,
    Stderr,
}

pub(crate) fn stream_lines<R>(
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

pub(crate) fn parse_worker_event(line: &str) -> Option<WorkerEvent> {
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

pub(crate) fn kill_child(child: &mut Child) -> Result<()> {
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
