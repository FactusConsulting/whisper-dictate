//! Integration tests for the `whisper-dictate inject` hidden subcommand.
//!
//! End-to-end exercises the JSON envelope contract that
//! `vp_inject_rust.inject_via_rust` shells out to: a request on stdin, a
//! JSON response on stdout. The `probe` action is safe to run in CI (no
//! display server / clipboard required) so it gives us a real subprocess
//! smoke test of the wire protocol.
//!
//! Default-feature build: the response reports `feature_enabled = false`.
//! The `rust-injection` variant is covered by the Rust unit tests in
//! `injection::dispatcher::tests` — we do NOT spawn enigo from here because
//! a display-less CI runner cannot construct one.

use std::io::Write;
use std::process::{Command, Stdio};

fn run_inject_with_stdin(stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .arg("inject")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn whisper-dictate inject");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait inject");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn inject_probe_returns_platform_metadata() {
    let (code, stdout, stderr) = run_inject_with_stdin(r#"{"action":"probe"}"#);
    assert_eq!(code, 0, "probe exited non-zero: stderr={stderr}");
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("inject probe must return JSON");
    assert!(parsed.get("platform").is_some(), "missing platform field");
    assert!(
        parsed.get("feature_enabled").is_some(),
        "missing feature_enabled field"
    );
}

#[test]
fn inject_rejects_invalid_json_envelope() {
    let (code, _stdout, stderr) = run_inject_with_stdin("not-json");
    assert_ne!(code, 0, "expected non-zero exit for invalid input");
    assert!(
        !stderr.is_empty(),
        "expected error on stderr for invalid envelope"
    );
}

#[test]
fn inject_rejects_unknown_action() {
    let (code, _stdout, stderr) = run_inject_with_stdin(r#"{"action":"summon"}"#);
    assert_ne!(code, 0, "unknown actions must fail");
    assert!(!stderr.is_empty(), "expected error on stderr");
}
