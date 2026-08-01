use std::fs;
use std::process::Command;

const WD: &str = env!("CARGO_BIN_EXE_wd");

use whisper_dictate_app::whisper::device_options::any_gpu_backend_compiled;

#[test]
fn config_set_vulkan_matches_native_build_capability() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    fs::write(&config, r#"{"device":"auto","model":"small"}"#).unwrap();

    let output = Command::new(WD)
        .args(["config", "set", "device", "vulkan"])
        .env("VOICEPI_CONFIG", &config)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    if any_gpu_backend_compiled() {
        assert!(output.status.success(), "stderr: {stderr}");
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(saved["device"], "vulkan");
        assert!(!stderr.contains("unavailable"));
    } else {
        assert!(!output.status.success());
        assert!(stderr.contains("Vulkan is unavailable"));
        assert!(stderr.contains("whisper-rs-vulkan"));
        assert!(!stderr.to_ascii_lowercase().contains("python"));
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(saved["device"], "auto", "rejected value must not persist");
    }
}

#[test]
fn config_set_auto_is_always_supported_without_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    fs::write(&config, r#"{"model":"small"}"#).unwrap();

    let output = Command::new(WD)
        .args(["config", "set", "device", "auto"])
        .env("VOICEPI_CONFIG", &config)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("warning:"));
}
