use super::test_support::{EnvVarGuard, ENV_LOCK};
use super::*;
use std::path::{Path, PathBuf};

fn value<'a>(command: &'a WorkerCommand, key: &str) -> Option<&'a str> {
    command.runtime_value(key)
}

#[test]
fn worker_command_is_a_native_in_process_configuration_envelope() {
    let command = worker_command("/tmp/whisper-dictate");

    assert!(command.args.is_empty());
    assert_eq!(command.working_dir, PathBuf::from("/tmp/whisper-dictate"));
    assert!(
        command.runtime_value_names().iter().all(|key| !matches!(
            key.as_str(),
            "PYTHONPATH" | "VOICEPI_PYTHON" | "VOICEPI_RUST_INJECTOR"
        )),
        "native runtime envelope must not carry retired worker launch controls"
    );
}

#[test]
fn worker_command_exports_effective_native_config() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _model = EnvVarGuard::set("VOICEPI_MODEL", "small");
    let command = worker_command("/tmp/whisper-dictate");

    assert_eq!(value(&command, "VOICEPI_MODEL"), Some("small"));
}

#[test]
fn worker_command_migrates_stale_groq_model_before_runtime_start() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(
        &config,
        serde_json::json!({
            "post_processor": "groq",
            "post_model": "llama-3.1-8b-instant"
        })
        .to_string(),
    )
    .unwrap();
    let _config = EnvVarGuard::set("VOICEPI_CONFIG", &config);

    let command = worker_command("/tmp/whisper-dictate");

    assert_eq!(
        value(&command, "VOICEPI_POST_MODEL"),
        Some(crate::config::DEFAULT_GROQ_POST_MODEL)
    );
}

#[test]
fn default_worker_command_honours_app_root_override() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _root = EnvVarGuard::set(APP_ROOT_ENV, "/installed/app");
    let command = default_worker_command();
    assert_eq!(command.working_dir, PathBuf::from("/installed/app"));
}

#[test]
fn cli_exe_resolution_maps_gui_names_to_wd() {
    let unix = cli_exe_from(Path::new("/opt/wd/wd-gui"));
    assert_eq!(unix, PathBuf::from("/opt/wd/wd"));

    let windows_style_name = cli_exe_from(Path::new("/opt/wd/wd-gui.exe"));
    assert_eq!(windows_style_name, PathBuf::from("/opt/wd/wd.exe"));

    let legacy_gui = cli_exe_from(Path::new("/opt/wd/whisper-dictate-gui.exe"));
    assert_eq!(legacy_gui, PathBuf::from("/opt/wd/wd.exe"));
}

#[test]
fn cli_exe_resolution_leaves_unknown_binary_names_unchanged() {
    let path = Path::new("/opt/wd/custom-launcher");
    assert_eq!(cli_exe_from(path), path);
}
