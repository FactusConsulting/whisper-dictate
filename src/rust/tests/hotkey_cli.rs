//! Integration tests for `whisper-dictate hotkey capture`.
//!
//! Exercises the CLI end-to-end (through `env!("CARGO_BIN_EXE_...")`) so the
//! stdout shape and exit-code contract stay pinned. The listener install may
//! or may not succeed depending on the platform + feature build:
//!
//! * **Stock build (no `rust-hotkeys`)** — install returns `Unsupported`,
//!   the process exits non-zero with a "rebuild with --features" hint. The
//!   tests below tolerate BOTH outcomes, asserting the shape rather than
//!   demanding a real listener that we can't guarantee on the CI runner.
//! * **Feature build on headless Linux** — rdev refuses (no X display),
//!   process exits non-zero with a "listener failed to start" hint (same
//!   P1-#2 refusal path the supervisor handles at runtime).
//! * **Feature build on Windows/macOS/a real user session** — the listener
//!   installs, the 0.1s window elapses, exit 0 with a `duration_reached`
//!   line.
//!
//! Whatever the outcome, output must be one JSON object per line for
//! `--json`, and the first line (or the error message) must contain enough
//! context that a smoke script can classify the run.

use std::process::{Command, Stdio};
use std::time::Duration;

/// Wall-clock budget for a single CLI invocation. Generous so a slow VM
/// doesn't spuriously fail — the actual `--for` window we ask for is 100 ms.
const RUN_TIMEOUT: Duration = Duration::from_secs(15);

fn run_capture(args: &[&str]) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .arg("hotkey")
        .arg("capture")
        .args(args)
        // Point the config at a nonexistent path so the process uses
        // AppSettings::default() (chord = "ctrl_r"). Prevents the test from
        // depending on whatever the user has in their real config file.
        .arg("--config")
        .arg("nonexistent-config-for-hotkey-capture-test.json")
        .env("VOICEPI_CONFIG", "nonexistent-for-hotkey-capture-test.json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn whisper-dictate hotkey capture");
    // Wait with a timeout so a hung listener can't wedge CI. We don't have
    // `wait_timeout` in the stdlib, so poll instead — the CLI window is
    // 100 ms, so the loop should exit almost immediately.
    let deadline = std::time::Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    panic!("hotkey capture did not exit within {RUN_TIMEOUT:?}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("try_wait error: {e}"),
        }
    }
    let output = child.wait_with_output().expect("collect output");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A very short `--for` window either lands in the "installed cleanly"
/// happy path (exit 0, first stdout line is the install envelope) or in the
/// "listener refused to start" path (non-zero exit, stderr carries a
/// classifiable hint). Anything else (exit 0 but no install envelope, or
/// non-zero exit with an unrecognised hint) is a regression the shipping
/// wayland-user-smoke script's classifier would trip on too.
#[test]
fn hotkey_capture_json_shape_or_classifiable_refusal() {
    let (code, stdout, stderr) = run_capture(&["--for", "0.1", "--json"]);
    if code == 0 {
        // Feature build on a real user session. First stdout line must be a
        // parseable JSON envelope with the listener_installed kind. Any
        // subsequent lines are optional (a 0.1s window with no typing is
        // usually empty apart from the terminal envelope).
        let first = stdout
            .lines()
            .next()
            .unwrap_or_else(|| panic!("no stdout — stderr: {stderr}"));
        let parsed: serde_json::Value = serde_json::from_str(first)
            .unwrap_or_else(|e| panic!("first line not JSON: {first} ({e})"));
        assert_eq!(
            parsed["kind"], "listener_installed",
            "first line kind: {parsed}"
        );
        assert!(
            parsed.get("driver").is_some(),
            "install envelope should include driver: {parsed}"
        );
        assert!(
            parsed.get("chord").is_some(),
            "install envelope should include chord: {parsed}"
        );
    } else {
        // Refusal — either the feature is missing (stock build) or the
        // listener refused to start (headless CI). Both cases must surface
        // a classifiable hint on stderr so the smoke-script warn-skip can
        // fire. The refusal messages come from `hotkey::capture::run_capture`
        // and `hotkey::install_hotkey_with_raw_tap`.
        let recognised = ["rust-hotkeys", "listener failed", "display", "permission"];
        assert!(
            recognised.iter().any(|hint| stderr.contains(hint)),
            "unclassifiable refusal — stderr must contain one of {recognised:?}: {stderr}"
        );
    }
}

/// Plain (non-JSON) output must carry the `[hotkey-capture]` prefix on
/// every emitted line. Same tolerance for stock/CI refusals as the JSON
/// test — we only assert the shape when the process managed to install.
#[test]
fn hotkey_capture_plain_output_uses_prefix() {
    let (code, stdout, _stderr) = run_capture(&["--for", "0.1"]);
    if code == 0 {
        for (idx, line) in stdout.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            // The duration-reached summary is a two-line block; the second
            // line is the "  Events: X ..." indent — that's fine, only the
            // primary emitted lines carry the prefix. Match either.
            assert!(
                line.starts_with("[hotkey-capture]") || line.starts_with("  "),
                "line {idx} missing prefix and not summary indent: {line:?}"
            );
        }
    }
}

/// `--for` must reject non-numeric values BEFORE the listener install runs
/// — deterministic behaviour across all platform / feature configurations.
#[test]
fn hotkey_capture_rejects_non_numeric_duration() {
    let (code, _stdout, stderr) = run_capture(&["--for", "not-a-number"]);
    assert_ne!(code, 0, "non-numeric --for must fail: stderr={stderr}");
    assert!(
        stderr.contains("numeric") || stderr.contains("--for"),
        "error should explain --for parse failure: {stderr}"
    );
}

/// `--for` must reject zero / negative values BEFORE the listener install
/// — same rationale as the non-numeric test.
#[test]
fn hotkey_capture_rejects_zero_duration() {
    let (code, _stdout, stderr) = run_capture(&["--for", "0"]);
    assert_ne!(code, 0, "zero --for must fail: stderr={stderr}");
    assert!(
        stderr.contains("positive") || stderr.contains("--for"),
        "error should explain zero rejection: {stderr}"
    );
}
