//! Bounded helper execution, including pipe I/O and process-tree cleanup.
//!
//! Helpers must be short-lived. The private Unix process group or Windows job
//! is terminated when the immediate child exits, times out, or is cancelled.
//! This is lifecycle management, not a sandbox for deliberately escaping code;
//! background services must not be launched through the ordinary runner.
//! Output is drained continuously but retained
//! only up to the caller's cap. No reader/writer thread survives a return.

use std::io;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "bounded_process_io.rs"]
mod pipes;
#[cfg(unix)]
#[path = "bounded_process_unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "bounded_process_windows.rs"]
mod platform;

pub(crate) const DIAGNOSTIC_LIMIT: usize = 64 * 1024;
pub(crate) const HELPER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Completion {
    Exited,
    TimedOut,
    Cancelled,
}

#[derive(Debug)]
pub(crate) struct Output {
    pub(crate) status: ExitStatus,
    pub(crate) completion: Completion,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stdout_truncated: bool,
    pub(crate) stdin_error: Option<io::Error>,
}

/// The deadline starts before spawn and covers writes, exit, and pipe EOF.
/// `should_continue` is polled only on the calling thread, never from I/O workers.
pub(crate) fn run(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    output_limit: usize,
    should_continue: &dyn Fn() -> bool,
) -> io::Result<Output> {
    run_with_policy(
        command,
        input,
        timeout,
        output_limit,
        should_continue,
        false,
    )
}

/// Clipboard tools deliberately leave a selection owner in the background.
/// Preserve it only after a successful parent exit and completed input; failures
/// and deadlines still terminate the whole tree. Callers must discard output
/// pipes so that the owner cannot retain them and delay EOF.
pub(crate) fn run_clipboard_owner(
    command: &mut Command,
    input: Vec<u8>,
    timeout: Duration,
) -> io::Result<Output> {
    run_with_policy(command, Some(input), timeout, 0, &|| true, true)
}

pub(crate) fn run_injection(
    command: &mut Command,
    input: Option<Vec<u8>>,
    helper: &str,
    should_continue: &dyn Fn() -> bool,
) -> anyhow::Result<Output> {
    if !should_continue() {
        anyhow::bail!("injection cancelled while running {helper}");
    }
    let output = run(
        command,
        input,
        HELPER_TIMEOUT,
        DIAGNOSTIC_LIMIT,
        should_continue,
    )?;
    match output.completion {
        Completion::Cancelled => anyhow::bail!("injection cancelled while running {helper}"),
        Completion::TimedOut => anyhow::bail!("{helper} injection timed out after 10s"),
        Completion::Exited => {
            if let Some(error) = output.stdin_error.as_ref() {
                anyhow::bail!("{helper} input failed: {error}");
            }
            Ok(output)
        }
    }
}

fn run_with_policy(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    output_limit: usize,
    should_continue: &dyn Fn() -> bool,
    allow_background_owner: bool,
) -> io::Result<Output> {
    let started = Instant::now();
    if !should_continue() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "helper cancelled before launch",
        ));
    }
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let (child, tree) = platform::spawn(command)?;
    let mut running = Running {
        child,
        tree,
        stop_io: Arc::new(AtomicBool::new(false)),
        workers: Vec::new(),
    };
    if let Some(stdout) = running.child.stdout.take() {
        running.workers.push((
            PipeKind::Stdout,
            pipes::Worker::reader(
                platform::pipe_file(stdout)?,
                Arc::clone(&running.stop_io),
                output_limit,
            )?,
        ));
    }
    if let Some(stderr) = running.child.stderr.take() {
        running.workers.push((
            PipeKind::Stderr,
            pipes::Worker::reader(
                platform::pipe_file(stderr)?,
                Arc::clone(&running.stop_io),
                output_limit,
            )?,
        ));
    }
    if let Some(input) = input {
        let stdin = running.child.stdin.take().expect("piped stdin");
        running.workers.push((
            PipeKind::Stdin,
            pipes::Worker::writer(
                platform::pipe_file(stdin)?,
                Arc::clone(&running.stop_io),
                input,
            )?,
        ));
    }
    let mut exited = None;
    let completion = loop {
        if !should_continue() {
            break Completion::Cancelled;
        }
        if started.elapsed() >= timeout {
            break Completion::TimedOut;
        }
        for (_, worker) in &mut running.workers {
            // In particular, close stdin as soon as its writer finishes so a
            // child reading to EOF can exit without waiting for our return.
            worker.release_finished_pipe();
        }
        if exited.is_none() {
            exited = running.child.try_wait()?;
            if exited.is_some_and(|status| !allow_background_owner || !status.success()) {
                // A descendant must not keep pipes open after its parent exits.
                running.tree.terminate();
            }
        }
        if exited.is_some()
            && running
                .workers
                .iter()
                .all(|(_, worker)| worker.is_finished())
        {
            break Completion::Exited;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if completion != Completion::Exited {
        running.stop_io.store(true, Ordering::Release);
        running.tree.terminate();
        let _ = running.child.kill();
    }
    let status = running.child.wait()?;
    let (mut stdout, mut stderr, mut stdin) = (
        pipes::Captured::default(),
        pipes::Captured::default(),
        pipes::Captured::default(),
    );
    for (kind, worker) in &mut running.workers {
        let result = worker.finish()?;
        match kind {
            PipeKind::Stdout => stdout = result,
            PipeKind::Stderr => stderr = result,
            PipeKind::Stdin => stdin = result,
        }
    }
    let stdin_error = stdin.error;
    let preserve_owner = completion == Completion::Exited
        && status.success()
        && allow_background_owner
        && stdin_error.is_none();
    if !preserve_owner {
        running.tree.terminate();
        running.tree.wait_terminated()?;
    }
    if completion == Completion::Exited {
        if let Some(error) = stdout.error.or(stderr.error) {
            return Err(error);
        }
        if preserve_owner {
            running.tree.disarm()?;
        }
    }
    Ok(Output {
        status,
        completion,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        stdout_truncated: stdout.truncated,
        stdin_error,
    })
}

struct Running {
    child: Child,
    tree: platform::ProcessTree,
    stop_io: Arc<AtomicBool>,
    workers: Vec<(PipeKind, pipes::Worker)>,
}

enum PipeKind {
    Stdout,
    Stderr,
    Stdin,
}

impl Drop for Running {
    fn drop(&mut self) {
        // Also covers try_wait errors, worker spawn failures, and panics.
        self.stop_io.store(true, Ordering::Release);
        self.tree.terminate();
        let _ = self.child.kill();
        let _ = self.child.wait();
        for (_, worker) in &mut self.workers {
            let _ = worker.finish();
        }
    }
}

#[cfg(test)]
#[path = "bounded_process_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "bounded_process_fixtures.rs"]
pub(crate) mod fixtures;
