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

use std::io::Write;
use std::process::{Command, Stdio};

const WD: &str = env!("CARGO_BIN_EXE_wd");
const WD_GUI: &str = env!("CARGO_BIN_EXE_wd-gui");
use std::time::Duration;

/// Wall-clock budget for a single CLI invocation. Generous so a slow VM
/// doesn't spuriously fail — the actual `--for` window we ask for is 100 ms.
const RUN_TIMEOUT: Duration = Duration::from_secs(15);

fn run_capture(args: &[&str]) -> (i32, String, String) {
    // Give every invocation its own push-to-talk ownership lock
    // (`hotkey::ptt_lock`). The real lock is per-USER, so without this the
    // tests in this file would contend with each other when cargo runs
    // them in parallel -- and with the developer's own running GUI, which
    // would turn a correct refusal into a red test.
    let lock_dir = tempfile::TempDir::new().expect("temp lock dir");
    let mut child = Command::new(WD)
        .arg("hotkey")
        .arg("capture")
        .args(args)
        .env("VOICEPI_PTT_LOCK_DIR", lock_dir.path())
        // Point the config at a nonexistent path so the process uses
        // AppSettings::default() (chord = "pause"). Prevents the test from
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

/// The guided verifier must execute inside the same binary as the UI parent.
/// Exercise the hidden dispatch through `wd-gui` itself; the invalid duration
/// fails before any OS hook is installed, so this remains deterministic on CI.
#[test]
fn gui_binary_dispatches_the_isolated_hotkey_probe_child() {
    let output = Command::new(WD_GUI)
        .args(["--internal-hotkey-probe", "hotkey", "capture", "--for", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run the GUI-subsystem hotkey probe child");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.contains("--for must be a positive finite number"),
        "GUI child did not dispatch to hotkey capture: {stderr}"
    );
}

#[test]
fn hotkey_capture_help_documents_nullable_focus_field() {
    let output = Command::new(WD)
        .args(["hotkey", "capture", "--help"])
        .output()
        .expect("run hotkey capture help");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "help failed: {stdout}");
    assert!(stdout.contains("nullable `focused`"), "help: {stdout}");
    assert!(stdout.contains("`null`"), "help: {stdout}");
}

#[test]
#[ignore = "child half of hotkey_probe_parent_pipe_exits_when_the_gui_disappears"]
fn hotkey_probe_parent_watchdog_child() {
    whisper_dictate_app::runtime::start_hotkey_probe_parent_watchdog()
        .expect("start production parent-pipe watchdog");
    println!("parent-watchdog-ready");
    std::io::stdout().flush().expect("flush readiness marker");
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn hotkey_probe_parent_pipe_exits_when_the_gui_disappears() {
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--ignored",
            "--exact",
            "hotkey_probe_parent_watchdog_child",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn parent-watchdog test child");
    let parent_pipe = child.stdin.take().expect("child stdin pipe");
    let stdout = child.stdout.take().expect("child stdout pipe");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;

        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if line.contains("parent-watchdog-ready") {
                let _ = ready_tx.send(());
                return;
            }
        }
    });

    if ready_rx.recv_timeout(Duration::from_secs(5)).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        panic!("parent-watchdog child did not become ready");
    }
    drop(parent_pipe);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait().expect("inspect parent-watchdog child") {
            Some(status) => break status,
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                panic!("hotkey probe survived after its parent pipe closed");
            }
        }
    };
    let _ = reader.join();
    assert!(status.success(), "watchdog exit status was {status}");
}
