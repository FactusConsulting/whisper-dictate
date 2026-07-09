//! Integration tests that survived Wave 8 Part 2.
//!
//! The pre-v1.20 supervisor lifecycle tests (start / stop / restart /
//! stream_lines / repaint notifier / env passthrough / worker-event
//! parse) spawned a plain `python -c "..."` child through the
//! supervisor's `WorkerCommand`. Wave 8 Part 2 made
//! `RuntimeSupervisor::start` UNCONDITIONALLY swap program+args to
//! `<current-exe> worker-rust` (via
//! `worker_rust::swap_command_to_worker_rust`) and REQUIRE the delegate
//! gate to approve first — which needs a resolvable GGML model and the
//! full feature set. Those preconditions cannot be met from a plain
//! integration test binary, and the whole point of the tests was that
//! the child WAS whatever the caller passed in. The supervisor's
//! start/stop/poll/stream_lines/notifier mechanics are still covered by
//! the module-level unit tests in `runtime/` (bridge_terminal_tests,
//! rust_session_sink_e2e_tests, external_toggle_tests, ...); they no
//! longer need an end-to-end Python subprocess.
//!
//! What stayed:
//!
//! * `windows_child_reads_piped_stdin_written_by_parent` — a low-level
//!   Windows named-pipe round-trip test. Bypasses the supervisor
//!   entirely (drives a bare `Command` with `Stdio::piped()` on stdin)
//!   so it still exercises the pre-Wave-8 pipe-handling regression the
//!   audio-in-rust bridge depended on.
//! * `cleanup_stale_desktop_processes_stops_worker_from_same_app_root`
//!   — exercises the Windows `cleanup_stale_desktop_processes` script.
//!   The current PowerShell matcher still targets the legacy
//!   `whisper_dictate.runtime` command-line pattern (an outdated
//!   check tracked as a Wave 8 Part 3 follow-up), but the test
//!   fixture spins up a real Python child matching that pattern so
//!   the code path still runs end-to-end.

// Wave 8 Part 2: every surviving test in this file is Windows-only
// (cross-platform supervisor tests moved to module-level unit tests
// in `runtime/`), so gate every import + helper on `windows` to keep
// stock-Linux/macOS builds warning-free under `-D warnings`.
#[cfg(windows)]
use std::env;
#[cfg(windows)]
use std::ffi::{OsStr, OsString};
#[cfg(windows)]
use std::fs;
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::process::{Child, Command};
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use whisper_dictate_app::runtime::cleanup_stale_desktop_processes;

/// Iteration-2 review finding #6: the audio-in-rust path adds
/// `Stdio::piped()` on the worker's stdin and the supervisor writes
/// to it from the bridge thread. Windows named-pipe semantics differ
/// from Unix (chunked writes, handle inheritance via
/// `STARTUPINFO.hStdInput`, hidden child window via
/// `CREATE_NO_WINDOW`), so we pin the round-trip end-to-end on
/// Windows CI here. The test does NOT depend on the `audio-in-rust`
/// cargo feature: it mirrors what the supervisor does — spawn a
/// child with piped stdin, write bytes, verify the child read them
/// and printed them back on stdout — so a regression in the
/// supervisor's Windows pipe-handling shows up even in default
/// builds.
#[cfg(windows)]
#[test]
fn windows_child_reads_piped_stdin_written_by_parent() {
    use std::io::Write;
    use std::process::Stdio;
    let Some(python) = test_python() else {
        return;
    };
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let mut child = Command::new(python)
        .args([
            "-c",
            // The child reads a line, prints it back, and exits.
            // sys.stdin.readline blocks until the parent writes — if
            // the Windows pipe handle wasn't inherited correctly the
            // read would never return and the test would time out.
            "import sys; line = sys.stdin.readline(); print(f'got:{line.strip()}', flush=True)",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("spawn python with piped stdin on Windows");

    {
        let stdin = child.stdin.as_mut().expect("piped stdin is Some");
        stdin
            .write_all(b"hello-from-rust\n")
            .expect("Windows pipe write succeeds");
        stdin.flush().expect("Windows pipe flush succeeds");
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("child terminates after stdin EOF");
    assert!(
        output.status.success(),
        "child exited non-zero on Windows: {:?}",
        output.status,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("got:hello-from-rust"),
        "Windows piped child must echo the parent's stdin line; got: {stdout:?}",
    );
}

#[cfg(windows)]
#[test]
fn cleanup_stale_desktop_processes_stops_worker_from_same_app_root() {
    let Some(python) = test_python() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let package = dir
        .path()
        .join("src")
        .join("python")
        .join("whisper_dictate");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("__init__.py"), "").unwrap();
    let worker = package.join("runtime.py");
    fs::write(
        &worker,
        "import time\nprint('worker-ready', flush=True)\ntime.sleep(60)\n",
    )
    .unwrap();
    let root_arg = dir.path().display().to_string();
    let mut child = Command::new(python)
        .args(["-m", "whisper_dictate.runtime", "--app-root", &root_arg])
        .current_dir(dir.path())
        .env("PYTHONPATH", dir.path().join("src").join("python"))
        .spawn()
        .unwrap();
    let _app_root_guard = EnvVarGuard::set("VOICEPI_APP_ROOT", dir.path());

    cleanup_stale_desktop_processes();

    assert!(
        wait_for_exit(&mut child, Duration::from_secs(5)),
        "stale worker should be stopped by startup cleanup"
    );
}

#[cfg(windows)]
fn test_python() -> Option<PathBuf> {
    for candidate in python_candidates() {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return Some(PathBuf::from(candidate));
        }
    }
    None
}

#[cfg(windows)]
fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    false
}

#[cfg(windows)]
struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

#[cfg(windows)]
impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(key);
        env::set_var(key, value);
        Self { key, original }
    }
}

#[cfg(windows)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            env::set_var(self.key, value);
        } else {
            env::remove_var(self.key);
        }
    }
}

#[cfg(windows)]
fn python_candidates() -> &'static [&'static str] {
    &["py.exe", "py", "python.exe", "python"]
}
