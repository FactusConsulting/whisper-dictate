//! Sibling tests for [`crate::runtime::external_toggle`] (issue #326).
//! Split out from the production file to keep both under the 500-LOC
//! modularity guideline (AGENTS.md). Follows the same pattern as
//! `audio_spawn_tests`, `rust_session_sink_*_tests` etc.

use super::external_toggle::*;
// The crate-wide `EnvVarGuard` (from `crate::test_env_lock`) replaced
// the previous file-local shim so every env-mutating test in the
// library binary uses one guard type and one poison-recovery pattern
// (Codex P2 #415 / #425). The guard's Drop restores the pre-existing
// value on panic, so a mid-test panic no longer leaks the override
// into every subsequent test — and the crate-wide `ENV_LOCK` serialises
// mutations across module boundaries, which a per-file guard could not.
#[cfg(target_os = "linux")]
use crate::test_env_lock::EnvVarGuard;
use std::io;

// --- Pure helpers: parse / serialize / signal mapping ---------------------

#[test]
fn parse_command_token_recognises_each_action() {
    assert_eq!(
        ExternalCommand::parse_command_token("toggle"),
        ExternalCommand::Toggle
    );
    assert_eq!(
        ExternalCommand::parse_command_token("start"),
        ExternalCommand::Start
    );
    assert_eq!(
        ExternalCommand::parse_command_token("stop"),
        ExternalCommand::Stop
    );
    assert_eq!(
        ExternalCommand::parse_command_token("cancel"),
        ExternalCommand::Cancel
    );
}

#[test]
fn parse_command_token_trims_whitespace() {
    assert_eq!(
        ExternalCommand::parse_command_token("  start \n"),
        ExternalCommand::Start
    );
}

#[test]
fn parse_command_token_defaults_unknown_to_toggle() {
    // Documented fallback so a malformed file (or a raw `kill -USR1` with no
    // companion file) does the obvious thing.
    assert_eq!(
        ExternalCommand::parse_command_token(""),
        ExternalCommand::Toggle
    );
    assert_eq!(
        ExternalCommand::parse_command_token("hop-and-jump"),
        ExternalCommand::Toggle
    );
}

#[test]
fn as_token_round_trips_through_parse() {
    for cmd in [
        ExternalCommand::Toggle,
        ExternalCommand::Start,
        ExternalCommand::Stop,
        ExternalCommand::Cancel,
    ] {
        assert_eq!(
            ExternalCommand::parse_command_token(cmd.as_token()),
            cmd,
            "round-trip for {cmd:?}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn to_signal_routes_cancel_to_sigusr2_and_rest_to_sigusr1() {
    assert_eq!(ExternalCommand::Toggle.to_signal(), libc::SIGUSR1);
    assert_eq!(ExternalCommand::Start.to_signal(), libc::SIGUSR1);
    assert_eq!(ExternalCommand::Stop.to_signal(), libc::SIGUSR1);
    assert_eq!(ExternalCommand::Cancel.to_signal(), libc::SIGUSR2);
}

// --- File I/O: PID file + command file ------------------------------------

#[test]
fn pid_file_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("nested").join("daemon.pid");
    write_pid_file(&path, 12345).expect("write");
    assert_eq!(read_pid_file(&path).expect("read"), 12345);
    cleanup_pid_file(&path);
    assert!(!path.exists());
}

#[test]
fn read_pid_file_rejects_garbage() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("daemon.pid");
    std::fs::write(&path, "not-a-pid").expect("write");
    let err = read_pid_file(&path).expect_err("garbage must error");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn take_command_token_consumes_file_and_returns_action() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("daemon.cmd");
    write_command_token(&path, ExternalCommand::Stop).expect("write");
    assert_eq!(take_command_token(&path), ExternalCommand::Stop);
    // Consumed: subsequent reads default to Toggle.
    assert!(!path.exists());
    assert_eq!(take_command_token(&path), ExternalCommand::Toggle);
}

#[cfg(target_os = "linux")]
#[test]
fn runtime_dir_honours_xdg_runtime_dir_on_linux() {
    // Take the crate-wide env lock so a concurrent test in another module
    // reading XDG_RUNTIME_DIR doesn't race our override.
    let _env = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let _g = EnvVarGuard::set("XDG_RUNTIME_DIR", tmp.path());
    let dir = runtime_dir().expect("xdg path");
    assert_eq!(dir, tmp.path().join("whisper-dictate"));
}

#[test]
fn default_pid_file_path_lives_under_runtime_dir() {
    // We only assert the suffix so the test is robust against the
    // platform-specific cache root.
    let Some(path) = default_pid_file_path() else {
        // CI containers can be missing every fallback env var; treat that
        // as "not applicable" rather than fail.
        eprintln!("skipping: no runtime dir resolvable in this env");
        return;
    };
    let display = path.display().to_string();
    assert!(
        display.contains("whisper-dictate"),
        "expected pid path to live under a whisper-dictate dir, got {display}"
    );
    assert!(
        display.ends_with("whisper-dictate.pid"),
        "expected pid file name suffix, got {display}"
    );
}

// --- Global channel -------------------------------------------------------

#[test]
fn push_and_take_round_trip_through_channel() {
    reset_channel_for_tests();
    push_command(ExternalCommand::Toggle);
    push_command(ExternalCommand::Cancel);
    let drained = take_pending_commands();
    assert_eq!(
        drained,
        vec![ExternalCommand::Toggle, ExternalCommand::Cancel]
    );
    // Second drain is empty (the channel was emptied).
    assert!(take_pending_commands().is_empty());
}

#[test]
fn recv_command_blocking_returns_pushed_command() {
    // Cross-platform coverage of `recv_command_blocking`: a pushed
    // command surfaces via the block-wait path (used by the Linux
    // e2e test to avoid poll+sleep starvation on CI). Runs on every
    // target so the helper stays live under `clippy -D warnings`.
    reset_channel_for_tests();
    push_command(ExternalCommand::Cancel);
    let recv = recv_command_blocking(std::time::Duration::from_secs(1));
    assert_eq!(recv, Some(ExternalCommand::Cancel));
    // A follow-up block-wait on the drained channel times out
    // rather than blocking forever, and returns None.
    let empty = recv_command_blocking(std::time::Duration::from_millis(20));
    assert_eq!(empty, None);
}

#[test]
fn recv_command_blocking_times_out_when_channel_empty() {
    // Second cross-platform pin: with nothing pushed the helper must
    // honour the timeout and yield None, not block indefinitely.
    reset_channel_for_tests();
    let started = std::time::Instant::now();
    let result = recv_command_blocking(std::time::Duration::from_millis(30));
    assert_eq!(result, None);
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(25),
        "recv_timeout should have honoured the 30ms budget, elapsed = {:?}",
        started.elapsed()
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn forward_command_returns_clear_not_implemented_on_non_linux() {
    let err = forward_command_to_pid(ExternalCommand::Toggle, 12345).expect_err("non-linux");
    assert!(
        err.to_string().contains("Linux-only"),
        "unexpected message: {err}"
    );
}

// --- Linux end-to-end: install signal handlers + raise on self ------------

/// Linux integration test: install the signal handlers, raise SIGUSR1
/// against ourselves, and assert the resulting [`ExternalCommand`] lands on
/// the in-process channel that the supervisor drains.
///
/// Single test combining the three signal paths because the signal-handler
/// thread is `OnceLock`-guarded — splitting into three tests would race on
/// the global state across cargo's parallel test runner. ENV_LOCK serialises
/// against any other test in the crate that mutates `XDG_RUNTIME_DIR`.
#[cfg(target_os = "linux")]
#[test]
fn linux_signal_handler_end_to_end() {
    let _env = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let _g = EnvVarGuard::set("XDG_RUNTIME_DIR", tmp.path());
    // First call installs; subsequent calls in the same process are a no-op
    // via the OnceLock guard inside install_linux_signal_handlers.
    let guard = install_signal_handlers()
        .expect("signal handlers should install in a normal Linux test env");
    assert!(
        guard.is_some(),
        "PID file path should resolve under XDG_RUNTIME_DIR"
    );
    // Verify the PID file was written.
    let pid_path = default_pid_file_path().expect("pid path");
    assert_eq!(
        read_pid_file(&pid_path).expect("pid file readable"),
        std::process::id()
    );
    reset_channel_for_tests();

    // --- Warmup: prove the signal→channel pipeline is live before the
    // real assertions. On a CPU-throttled CI runner (observed
    // repeatedly on rust (ubuntu-latest), PR #428) the signal-handler
    // thread may not be scheduled inside the parent's poll window even
    // though the sigaction registered by `Signals::new` has already
    // captured the signal into signal-hook's self-pipe. The readiness
    // handshake in `install_linux_signal_handlers` guarantees the
    // thread has BEEN scheduled once, but not that it has entered the
    // blocking iterator; the warmup raises a signal and blocks on the
    // channel with the full generous timeout, which forces the OS to
    // schedule the handler thread at least until it drains one signal.
    // Subsequent raises then land near-instantly because the thread is
    // already blocked inside `signals.forever()`.
    // SAFETY: libc::raise is a thin syscall wrapper on a validated int.
    unsafe {
        assert_eq!(
            libc::raise(libc::SIGUSR1),
            0,
            "warmup raise SIGUSR1 must succeed"
        );
    }
    let warmup = wait_for_command();
    assert_eq!(
        warmup,
        Some(ExternalCommand::Toggle),
        "warmup: signal-handler thread did not surface a Toggle within the poll budget \
         — the sigaction is registered synchronously so a None here means the handler \
         thread never got scheduled to drain the self-pipe"
    );
    reset_channel_for_tests();

    // --- SIGUSR1 with no command file → Toggle (documented default).
    // SAFETY: libc::raise is a thin syscall wrapper.
    unsafe {
        assert_eq!(libc::raise(libc::SIGUSR1), 0, "raise SIGUSR1 must succeed");
    }
    let cmd = wait_for_command();
    assert_eq!(cmd, Some(ExternalCommand::Toggle));

    // --- SIGUSR1 with command file = "stop" → Stop.
    let cmd_path = default_command_file_path().expect("cmd path");
    write_command_token(&cmd_path, ExternalCommand::Stop).expect("write token");
    unsafe {
        assert_eq!(libc::raise(libc::SIGUSR1), 0);
    }
    let cmd = wait_for_command();
    assert_eq!(cmd, Some(ExternalCommand::Stop));

    // --- SIGUSR2 → Cancel regardless of command file.
    write_command_token(&cmd_path, ExternalCommand::Toggle).expect("write toggle token");
    unsafe {
        assert_eq!(libc::raise(libc::SIGUSR2), 0);
    }
    let cmd = wait_for_command();
    assert_eq!(cmd, Some(ExternalCommand::Cancel));
}

/// Block-wait on the global channel for a single command. Used by
/// [`linux_signal_handler_end_to_end`] to wait for the signal-handler
/// thread to push without spinning forever if the test is broken.
///
/// Uses `recv_command_blocking` (a `recv_timeout` on the mpsc receiver)
/// rather than the earlier poll+sleep implementation. Prior versions
/// (5s poll @ 10ms interval) still flaked on rust (ubuntu-latest)
/// because the parent thread was spinning through `try_recv`+`sleep`
/// while the OS scheduler kept the signal-handler thread starved. A
/// blocking `recv_timeout` yields the CPU to the handler immediately
/// and gives it a real chance to enter `signals.forever()`.next() and
/// push the command.
///
/// The sigaction registered by `Signals::new` is synchronous, so a
/// timeout here still indicates scheduler starvation on the handler
/// thread, never a lost signal.
#[cfg(target_os = "linux")]
fn wait_for_command() -> Option<ExternalCommand> {
    crate::runtime::external_toggle::recv_command_blocking(std::time::Duration::from_secs(10))
}
