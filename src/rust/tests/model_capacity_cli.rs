#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use serde_json::Value;

fn capacity_report(free_mib: u32, exit_code: u8) -> Value {
    let fixture = tempfile::tempdir().unwrap();
    let nvidia_smi = fixture.path().join("nvidia-smi");
    fs::write(
        &nvidia_smi,
        format!(
            "#!/bin/sh\n\
             [ \"$1\" = --query-gpu=index,name,memory.total,memory.free ] || exit 64\n\
             [ \"$2\" = --format=csv,noheader,nounits ] || exit 64\n\
             printf '%s\\n' '3, Fixture NVIDIA adapter, 8192, {free_mib}'\n\
             exit {exit_code}\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&nvidia_smi, fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wd"))
        .args(["model-capacity", "--json"])
        .env("PATH", fixture.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn live_nvidia_headroom_changes_model_fit_without_changing_total_capacity() {
    for (free, expected) in [(1554, "free-vram"), (4800, "ok")] {
        let report = capacity_report(free, 0);
        let nvidia = report["gpus"]
            .as_array()
            .unwrap()
            .iter()
            .find(|gpu| gpu["source"] == "nvidia-smi")
            .expect("successful NVIDIA query should be reported");
        assert_eq!(nvidia["total_mb"], 8192);
        assert_eq!(nvidia["free_mb"], free);
        let fit = report["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["name"] == "Ollama Qwen2.5 3B")
            .unwrap();
        assert_eq!(fit["status"], expected);
    }
}

#[test]
fn failed_nvidia_query_cannot_report_a_successful_fit_from_stdout() {
    let report = capacity_report(8000, 1);
    assert!(report["gpus"]
        .as_array()
        .unwrap()
        .iter()
        .all(|gpu| gpu["source"] != "nvidia-smi"));
    let fit = report["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["name"] == "Ollama Qwen2.5 3B")
        .unwrap();
    assert_eq!(fit["status"], "too-small");
}
