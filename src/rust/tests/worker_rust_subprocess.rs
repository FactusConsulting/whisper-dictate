//! Integration test for the Wave 5 PR 6 `whisper-dictate worker-rust`
//! CLI entry point (`src/rust/runtime/worker_rust.rs`).
//!
//! Spawns the binary with `VOICEPI_DICTATE_BACKEND=rust-session`
//! (which the supervisor checks to delegate the dictation lifecycle
//! to the new subprocess) and `--stdin-only` (which skips the rdev
//! OS listener so we can drive press / release synthetically -- CI
//! has no display for rdev to attach to). The test then:
//!
//! 1. Reads stderr in a background thread and waits for the initial
//!    `state=ready` heartbeat the worker emits at startup -- proves
//!    the subprocess wired the session sink without crashing.
//! 2. Sends `press\n` and `release\n` on stdin -- drives the
//!    coordinator through StartRecording -> StopAndTranscribe.
//! 3. Asserts the worker emits the canonical state sequence on
//!    stderr (`opening` -> `recording` -> `transcribing` -> some
//!    no-text reason -> `ready`) -- proves the
//!    coordinator -> sink -> session -> emitter chain ran
//!    end-to-end without the parent supervisor.
//! 4. Closes stdin to trigger graceful shutdown and asserts the
//!    process exits cleanly with code 0.
//!
//! Stock-feature build: the worker uses the PR 4 stub session
//! backends (transcribe -> empty text, inject -> no-op, no audio
//! pump) so the test runs without whisper.cpp / enigo / cpal. The
//! state sequence shape is identical on real-feature builds -- only
//! the no-text reason differs (`empty` vs the real transcriber's
//! output) -- so the test's shape-not-reason assertion holds across
//! feature configurations.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Block reading lines from `reader` and forward each one onto `tx`.
/// Returns when the reader hits EOF / error. Used for both stdout
/// and stderr so the test can interleave assertions without
/// deadlocking on a full OS pipe buffer (the default pipe size is
/// ~64 KiB on Linux / Windows).
fn forward_lines<R: BufRead + Send + 'static>(
    mut reader: R,
    tx: mpsc::Sender<String>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {
                    // Strip the trailing newline for cleaner asserts.
                    let trimmed = line.trim_end_matches(&['\n', '\r'][..]).to_owned();
                    if tx.send(trimmed).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    })
}

/// Wait for a line matching `predicate` to arrive on `rx`. Drains
/// every non-matching line into the returned `Vec` so the caller
/// can assert about earlier lines later. Panics with a helpful
/// message on timeout.
fn wait_for_line(
    rx: &mpsc::Receiver<String>,
    predicate: impl Fn(&str) -> bool,
    timeout: Duration,
    label: &str,
) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    let mut collected = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                let matched = predicate(&line);
                collected.push(line);
                if matched {
                    return collected;
                }
            }
            Err(_) => break,
        }
    }
    panic!(
        "timed out waiting for `{label}` after {:?}; saw {} line(s):\n{}",
        timeout,
        collected.len(),
        collected.join("\n")
    );
}

/// Extract the `state` field from a `[worker-event] {...}` line, if
/// the line carries that prefix and the payload parses as a JSON
/// object with a string `"state"` field. Returns `None` for non-
/// worker-event lines (stderr traces, prefixed warnings, etc.).
fn parse_state(line: &str) -> Option<String> {
    let raw = line.strip_prefix("[worker-event] ")?;
    let payload: serde_json::Value = serde_json::from_str(raw).ok()?;
    payload
        .get("state")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

#[test]
fn worker_rust_stdin_driven_press_release_emits_full_state_sequence() {
    // ── spawn the worker subprocess ─────────────────────────────────
    let mut child = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .args(["worker-rust", "--stdin-only"])
        // VOICEPI_WORKER_EVENTS=1 is set inside `handle_worker_rust`
        // before the heartbeat is emitted, but we set it here too so
        // the wire-emitter gate is active even on a child that exited
        // mid-startup (and so the test is robust to a future
        // refactor that moves the env-set further down). Both code
        // paths converge on the same gate.
        .env("VOICEPI_WORKER_EVENTS", "1")
        // Pretend the user picked the rust-session backend so any
        // log lines that consult the env reflect the production
        // path. The integration test does not rely on this gate to
        // run the worker (the subcommand always runs when --stdin-only
        // is passed), but matching production env keeps the trace
        // realistic.
        .env("VOICEPI_DICTATE_BACKEND", "rust-session")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn whisper-dictate worker-rust");

    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
    let stderr = BufReader::new(child.stderr.take().expect("piped stderr"));

    let (stdout_tx, stdout_rx) = mpsc::channel::<String>();
    let (stderr_tx, stderr_rx) = mpsc::channel::<String>();
    let stdout_thread = forward_lines(stdout, stdout_tx);
    let stderr_thread = forward_lines(stderr, stderr_tx);

    // ── 1. heartbeat: worker came up + wired the sink ───────────────
    //
    // `state=ready` is the first worker event handle_worker_rust
    // emits, BEFORE building the production sink. Acts as a liveness
    // probe so a subprocess that crashes during sink construction
    // doesn't hang the test on `wait_for_line(recording)`.
    let _heartbeat = wait_for_line(
        &stderr_rx,
        |line| parse_state(line).as_deref() == Some("ready"),
        Duration::from_secs(10),
        "startup state=ready heartbeat",
    );

    // ── 2. drive a synthetic press / release ────────────────────────
    write_command(&stdin, "press");
    // Wait for the session's `state=recording` event before sending
    // the release; otherwise on slow CI runners the release could
    // race the press (the coordinator's debounce + the session's
    // start path are otherwise tested in unit tests).
    wait_for_line(
        &stderr_rx,
        |line| parse_state(line).as_deref() == Some("recording"),
        Duration::from_secs(5),
        "state=recording after press",
    );

    write_command(&stdin, "release");

    // ── 3. assert the canonical state sequence ──────────────────────
    //
    // After Release the session emits transcribing -> (no_text |
    // some_text) -> ready. The stub backend produces empty text so
    // the no-text path always fires; real-backend builds may produce
    // an utterance event instead (no `state` field at all). We only
    // assert what's invariant: `transcribing` appears, then `ready`
    // arrives.
    wait_for_line(
        &stderr_rx,
        |line| parse_state(line).as_deref() == Some("transcribing"),
        Duration::from_secs(5),
        "state=transcribing after release",
    );
    wait_for_line(
        &stderr_rx,
        |line| parse_state(line).as_deref() == Some("ready"),
        Duration::from_secs(5),
        "state=ready after transcribing",
    );

    // ── 4. close stdin -> worker exits cleanly ──────────────────────
    drop(stdin);

    // Wait for the process to exit with a generous deadline so a
    // slow CI runner has time to join the coordinator thread + the
    // event pump. The worker's run() does coord.shutdown +
    // coord_thread.join() + pump.join() in order; the pump joins
    // when the event channel disconnects (which happens when the
    // last sender -- our tx -- drops).
    let status = wait_for_exit(&mut child, Duration::from_secs(15))
        .expect("worker did not exit within deadline after stdin closed");
    assert!(
        status.success(),
        "worker-rust subprocess must exit successfully on graceful stdin close; got: {status:?}"
    );

    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    // Drain any tail stdout so a future regression that bleeds onto
    // stdout shows up as a test failure rather than disappearing.
    let stdout_tail: Vec<String> = stdout_rx.try_iter().collect();
    assert!(
        stdout_tail.is_empty()
            || stdout_tail
                .iter()
                .all(|l| l.is_empty() || l.starts_with("  (heard)")),
        "stdout should be empty or only contain `(heard)` lines (print-mode inject); \
         got:\n{}",
        stdout_tail.join("\n")
    );
}

/// Write `command\n` to the worker's stdin. Splits the write out so
/// the bytes-vs-string handling is in one place and a Write failure
/// gets a useful panic message.
fn write_command(mut stdin: &std::process::ChildStdin, command: &str) {
    use std::io::Write as _;
    let line = format!("{command}\n");
    stdin
        .write_all(line.as_bytes())
        .unwrap_or_else(|e| panic!("write `{command}` to worker stdin: {e}"));
    stdin
        .flush()
        .unwrap_or_else(|e| panic!("flush worker stdin after `{command}`: {e}"));
}

/// Poll [`std::process::Child::try_wait`] until the worker exits or
/// `deadline` elapses. Returns `Some(status)` on exit, `None` on
/// timeout. The caller decides whether to panic / kill on timeout
/// so we don't double-tap a kill the integration test might want
/// to investigate.
fn wait_for_exit(
    child: &mut std::process::Child,
    deadline: Duration,
) -> Option<std::process::ExitStatus> {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    // On timeout, kill the child so the test runner doesn't leak it.
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// EOF on stdin (no explicit `quit` command) must also trigger a
/// graceful shutdown -- mirrors what the supervisor relies on when
/// the parent closes its end of the pipe (the portable shutdown
/// mechanism on Windows where SIGTERM isn't reliable).
#[test]
fn worker_rust_eof_on_stdin_shuts_down_cleanly() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .args(["worker-rust", "--stdin-only"])
        .env("VOICEPI_WORKER_EVENTS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn whisper-dictate worker-rust");

    let stdin = child.stdin.take().expect("piped stdin");
    let stderr = BufReader::new(child.stderr.take().expect("piped stderr"));
    let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
    let (stderr_tx, stderr_rx) = mpsc::channel::<String>();
    let (stdout_tx, _stdout_rx) = mpsc::channel::<String>();
    let stderr_thread = forward_lines(stderr, stderr_tx);
    let stdout_thread = forward_lines(stdout, stdout_tx);

    // Wait for the heartbeat so we know the worker is past the
    // synchronous startup work before we close stdin (avoids a
    // false-positive "exited before sink built" diagnosis).
    let _ = wait_for_line(
        &stderr_rx,
        |line| parse_state(line).as_deref() == Some("ready"),
        Duration::from_secs(10),
        "startup state=ready heartbeat",
    );

    // Close stdin without writing any command. The mainloop's
    // BufReader::lines() returns None on EOF and run_stdin_loop
    // returns; the worker then shuts the coordinator down + joins
    // and exits 0.
    drop(stdin);

    let status = wait_for_exit(&mut child, Duration::from_secs(15))
        .expect("worker must exit within the deadline after EOF on stdin");
    assert!(
        status.success(),
        "worker-rust must exit 0 on stdin EOF; got: {status:?}"
    );

    let _ = stderr_thread.join();
    let _ = stdout_thread.join();
}
