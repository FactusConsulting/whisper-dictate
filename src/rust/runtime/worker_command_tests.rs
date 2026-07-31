use super::test_support::{EnvVarGuard, ENV_LOCK};
use super::*;
use std::path::{Path, PathBuf};

fn value<'a>(command: &'a WorkerCommand, key: &str) -> Option<&'a str> {
    command
        .env
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

#[test]
fn worker_command_constructs_the_legacy_utility_runtime_shape() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _python = EnvVarGuard::set(PYTHON_ENV, "python-test");
    let command = worker_command("/tmp/whisper-dictate");

    assert_eq!(command.program, PathBuf::from("python-test"));
    assert_eq!(&command.args[..2], ["-m", "whisper_dictate.runtime"]);
    assert!(command.args.contains(&"--app-root".to_owned()));
    assert_eq!(command.working_dir, PathBuf::from("/tmp/whisper-dictate"));
}

#[test]
fn worker_command_appends_passthrough_arguments() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _python = EnvVarGuard::set(PYTHON_ENV, "python-test");
    let command = worker_command_with_args(
        "/tmp/whisper-dictate",
        vec!["--doctor".to_owned(), "--json".to_owned()],
    );
    assert!(command
        .args
        .ends_with(&["--doctor".into(), "--json".into()]));
}

#[test]
fn worker_command_exports_effective_config_and_cli_path() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _python = EnvVarGuard::set(PYTHON_ENV, "python-test");
    let _model = EnvVarGuard::set("VOICEPI_MODEL", "small");
    let command = worker_command("/tmp/whisper-dictate");

    assert_eq!(value(&command, "VOICEPI_MODEL"), Some("small"));
    assert!(value(&command, worker_command::RUST_INJECTOR_ENV).is_some());
    assert!(value(&command, PYTHONPATH_ENV).is_some());
}

#[test]
fn doctor_command_keeps_its_runtime_argument() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _python = EnvVarGuard::set(PYTHON_ENV, "python-test");
    let command = doctor_command();
    assert!(command.args.contains(&"--doctor".to_owned()));
}

#[test]
fn default_worker_command_honours_app_root_override() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _python = EnvVarGuard::set(PYTHON_ENV, "python-test");
    let _root = EnvVarGuard::set(APP_ROOT_ENV, "/installed/app");
    let command = default_worker_command();
    assert_eq!(command.working_dir, PathBuf::from("/installed/app"));
}

#[test]
fn audio_devices_command_propagates_native_device_backend() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _python = EnvVarGuard::set(PYTHON_ENV, "python-test");
    let _audio = EnvVarGuard::set(AUDIO_BACKEND_ENV, "rust");
    let command = audio_devices_command();
    assert_eq!(value(&command, "VOICEPI_DEVICES_BACKEND"), Some("rust"));
}

#[test]
fn audio_devices_command_preserves_an_explicit_device_backend() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _python = EnvVarGuard::set(PYTHON_ENV, "python-test");
    let _audio = EnvVarGuard::set(AUDIO_BACKEND_ENV, "rust");
    let _devices = EnvVarGuard::set("VOICEPI_DEVICES_BACKEND", "custom");
    let command = audio_devices_command();
    assert_ne!(value(&command, "VOICEPI_DEVICES_BACKEND"), Some("rust"));
}

#[test]
fn cli_exe_resolution_maps_gui_to_sibling_binary() {
    let unix = cli_exe_from(Path::new("/opt/wd/whisper-dictate-gui"));
    assert_eq!(unix, PathBuf::from("/opt/wd/whisper-dictate"));

    let windows_style_name = cli_exe_from(Path::new("/opt/wd/whisper-dictate-gui.exe"));
    assert_eq!(
        windows_style_name,
        PathBuf::from("/opt/wd/whisper-dictate.exe")
    );
}

#[test]
fn cli_exe_resolution_leaves_unknown_binary_names_unchanged() {
    let path = Path::new("/opt/wd/custom-launcher");
    assert_eq!(cli_exe_from(path), path);
}
