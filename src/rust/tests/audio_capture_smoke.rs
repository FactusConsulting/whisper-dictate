//! CI may skip missing hardware, but must not swallow build/runtime regressions.
#![cfg(target_os = "linux")]

mod common;

use serde_json::{json, Value};
use std::process::Command;

fn report() -> Value {
    json!({"kind":"audio_capture_self_test", "requested_duration_ms":500,
        "succeeded":true, "error":"", "frames_captured":2,
        "samples_captured":960, "sample_rate":48000})
}

fn accepted(body: &str, status: i32) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("report.json");
    std::fs::write(&path, body).unwrap();
    Command::new("bash")
        .arg(common::repo_root().join("scripts/integration/check-audio-capture-report.sh"))
        .arg(path)
        .arg(status.to_string())
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn audio_smoke_accepts_success_and_only_explicit_missing_hardware() {
    assert!(accepted(&report().to_string(), 0));
    let mut missing = report();
    missing["succeeded"] = json!(false);
    missing["frames_captured"] = json!(0);
    missing["samples_captured"] = json!(0);
    missing["sample_rate"] = json!(0);
    for error in [
        "open capture: no default input device available",
        "open capture: supported_input_configs: The requested audio device is not available. It may have been disconnected.",
    ] {
        missing["error"] = json!(error);
        assert!(accepted(&missing.to_string(), 1));
        assert!(!accepted(&missing.to_string(), 0));
    }
}

#[test]
fn audio_smoke_rejects_panics_errors_and_malformed_or_missing_reports() {
    for body in ["", "not JSON", "{}", "[]", "{\"kind\":", "null"] {
        assert!(!accepted(body, 0));
        assert!(!accepted(body, 1));
    }
    assert!(!accepted(&report().to_string(), 101));
    assert!(!accepted(&report().to_string(), 127));
    assert!(!accepted(&format!("{}\n{}", report(), report()), 0));
    for (key, value) in [
        ("kind", json!("wrong")),
        ("succeeded", json!(false)),
        ("succeeded", json!(1)),
        ("frames_captured", json!(-1)),
        ("samples_captured", json!(0)),
        ("sample_rate", json!("48000")),
        ("requested_duration_ms", json!(0)),
        ("error", json!(null)),
    ] {
        let mut bad = report();
        bad[key] = value;
        assert!(!accepted(&bad.to_string(), 0), "{key}");
    }
    let mut stalled = report();
    stalled["succeeded"] = json!(false);
    stalled["frames_captured"] = json!(0);
    stalled["samples_captured"] = json!(0);
    stalled["error"] = json!("no audio device delivered samples within 500 ms - check permissions, mic mute state, or PIPEWIRE_QUANTUM");
    assert!(!accepted(&stalled.to_string(), 1));
    stalled["sample_rate"] = json!(0);
    stalled["error"] = json!("open capture: unexpected backend error");
    assert!(!accepted(&stalled.to_string(), 1));
}

#[test]
fn audio_smoke_missing_executable_is_not_a_hardware_skip() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new("bash")
        .arg(common::repo_root().join("scripts/integration/audio-capture-smoke.sh"))
        .arg(dir.path().join("missing-wd"))
        .output()
        .unwrap();
    assert!(!output.status.success());
}
