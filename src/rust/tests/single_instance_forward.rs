//! Cross-process integration test for the issue #327 single-instance
//! gate. Spawns two real `whisper-dictate` processes via the hidden
//! `single-instance-probe` subcommand:
//!
//! 1. Process A runs `single-instance-probe --serve-ms 3000` — it
//!    acquires the lock and prints `[acquired] port=… pid=…` on its
//!    stdout, then prints `[forwarded] <json>` for every forwarded
//!    argv it receives during its serve window.
//! 2. Process B runs `single-instance-probe -- --toggle-recording` —
//!    it should see the existing lockfile, forward its argv to
//!    process A, and exit 0 with `[forwarded]` on its stdout.
//!
//! Both processes share a per-test tempdir via
//! `VOICEPI_SINGLE_INSTANCE_DIR` so this test never touches a real
//! `$XDG_RUNTIME_DIR` and can run in parallel with other tests.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_whisper-dictate")
}

/// Spawn process A (the server) and block until it prints its
/// `[acquired] port=… pid=…` line so we know the port is bound. Return
/// the running child + a receiver that yields every stdout line the
/// server prints during its serve window.
fn spawn_server(
    runtime_dir: &std::path::Path,
    serve_ms: u64,
) -> (std::process::Child, mpsc::Receiver<String>) {
    let mut child = Command::new(bin())
        .args(["single-instance-probe", "--serve-ms", &serve_ms.to_string()])
        .env("VOICEPI_SINGLE_INSTANCE_DIR", runtime_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn server");

    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });

    // Wait for the readiness banner. Fail loud on timeout so a genuine
    // regression doesn't hang CI.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("server did not print readiness banner within 10s");
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) if line.starts_with("[acquired]") => break,
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                panic!("server exited before printing readiness banner");
            }
        }
    }

    (child, rx)
}

#[test]
fn second_process_forwards_argv_to_first_and_exits() {
    let runtime_dir = TempDir::new().expect("tempdir");
    let (mut server, server_stdout) = spawn_server(runtime_dir.path(), 3_000);

    // Second process forwards `--toggle-recording` and should exit 0
    // quickly with `[forwarded]` on stdout.
    let client_out = Command::new(bin())
        .args(["single-instance-probe", "--", "--toggle-recording"])
        .env("VOICEPI_SINGLE_INSTANCE_DIR", runtime_dir.path())
        .output()
        .expect("client output");
    assert!(
        client_out.status.success(),
        "client exited non-zero: status={:?} stderr={:?}",
        client_out.status,
        String::from_utf8_lossy(&client_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&client_out.stdout);
    assert!(
        stdout.contains("[forwarded]"),
        "expected client stdout to contain [forwarded], got: {stdout}"
    );

    // Server should have received the forwarded argv on its stdout.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut received = None;
    while std::time::Instant::now() < deadline {
        match server_stdout.recv_timeout(Duration::from_millis(200)) {
            Ok(line) if line.starts_with("[forwarded]") => {
                received = Some(line);
                break;
            }
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // Reap the server before asserting so a failure doesn't leave a
    // zombie behind on the CI runner.
    let _ = server.kill();
    let _ = server.wait();

    let line = received.expect("server never surfaced a [forwarded] line");
    assert!(
        line.contains("--toggle-recording"),
        "expected forwarded argv in server line, got: {line}"
    );
}

#[test]
fn client_with_no_running_instance_acquires_then_exits() {
    let runtime_dir = TempDir::new().expect("tempdir");
    // No server running — the client should discover no lockfile,
    // acquire the lock itself, release it on drop, and exit 0.
    let out = Command::new(bin())
        .args(["single-instance-probe", "--", "--noop"])
        .env("VOICEPI_SINGLE_INSTANCE_DIR", runtime_dir.path())
        .output()
        .expect("client output");
    assert!(
        out.status.success(),
        "unexpected non-zero exit: {:?} stderr={:?}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[acquired]"),
        "expected [acquired], got: {stdout}"
    );
}
