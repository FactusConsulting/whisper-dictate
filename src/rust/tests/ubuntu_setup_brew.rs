#![cfg(target_os = "linux")]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

fn run_brew_stage(installed: bool, upgrade_status: i32, message: &str) -> (Output, String) {
    let dir = tempfile::tempdir().unwrap();
    let brew = dir.path().join("brew");
    std::fs::write(
        &brew,
        r#"#!/bin/bash
set -eu
echo "$1" >> "$WD_TEST_BREW_LOG"
case "$1" in
  tap) exit 0 ;;
  list) exit "$WD_TEST_INSTALLED_STATUS" ;;
  install) echo 'fixture installed'; exit 0 ;;
  upgrade) echo "$WD_TEST_UPGRADE_MESSAGE" >&2; exit "$WD_TEST_UPGRADE_STATUS" ;;
  *) echo "unexpected brew operation: $1" >&2; exit 90 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&brew, std::fs::Permissions::from_mode(0o755)).unwrap();
    let script =
        std::fs::read_to_string(common::repo_root().join("packaging/linux/ubuntu26.04/setup.sh"))
            .unwrap();
    // Exercise the real Homebrew stage, stopping before any system changes.
    let (stage, _) = script.split_once("step \"evdev: gcc-12").unwrap();
    let log = dir.path().join("brew.log");
    let output = Command::new("bash")
        .args(["-c", stage])
        .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
        .env("WD_TEST_BREW_LOG", &log)
        .env(
            "WD_TEST_INSTALLED_STATUS",
            if installed { "0" } else { "1" },
        )
        .env("WD_TEST_UPGRADE_STATUS", upgrade_status.to_string())
        .env("WD_TEST_UPGRADE_MESSAGE", message)
        .output()
        .unwrap();
    (output, std::fs::read_to_string(log).unwrap())
}

#[test]
fn setup_propagates_upgrade_failure_and_keeps_its_diagnostic() {
    let (output, log) = run_brew_stage(true, 42, "fixture network or permissions failure");
    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("fixture network or permissions failure")
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("allerede nyeste"));
    assert_eq!(log.lines().collect::<Vec<_>>(), ["tap", "list", "upgrade"]);
}

#[test]
fn setup_accepts_successful_upgrade_and_already_current_noop() {
    for message in ["fixture upgraded", "fixture already installed"] {
        let (output, log) = run_brew_stage(true, 0, message);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        assert_eq!(log.lines().collect::<Vec<_>>(), ["tap", "list", "upgrade"]);
    }
}

#[test]
fn setup_installs_when_formula_is_missing() {
    let (output, log) = run_brew_stage(false, 42, "must not upgrade");
    assert!(output.status.success());
    assert_eq!(log.lines().collect::<Vec<_>>(), ["tap", "list", "install"]);
}
