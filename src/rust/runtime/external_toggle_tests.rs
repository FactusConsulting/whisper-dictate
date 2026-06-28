//! Sibling tests for [`crate::runtime::external_toggle`] (issue #326).
//! Split out from the production file to keep both under the 500-LOC
//! modularity guideline (AGENTS.md). Follows the same pattern as
//! `audio_spawn_tests`, `rust_session_sink_*_tests` etc.

use super::external_toggle::*;
#[cfg(target_os = "linux")]
use std::ffi::{OsStr, OsString};
use std::io;

/// RAII wrapper around an env var so tests don't pollute the process env.
/// Linux-only because only the Linux test paths mutate env vars.
#[cfg(target_os = "linux")]
struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

#[cfg(target_os = "linux")]
impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }
}

#[cfg(target_os = "linux")]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

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

/// Poll the global channel for up to ~1 s waiting for a single command. Used
/// by [`linux_signal_handler_end_to_end`] to wait for the signal-handler
/// thread to push without spinning forever if the test is broken.
#[cfg(target_os = "linux")]
fn wait_for_command() -> Option<ExternalCommand> {
    for _ in 0..100 {
        let cmds = take_pending_commands();
        if let Some(first) = cmds.into_iter().next() {
            return Some(first);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    None
}
