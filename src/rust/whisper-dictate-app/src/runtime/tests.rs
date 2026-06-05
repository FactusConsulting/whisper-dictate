use super::*;
use std::ffi::{OsStr, OsString};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn runtime_module_args() -> Vec<String> {
    vec![
        "-m".to_owned(),
        "whisper_dictate.runtime".to_owned(),
        "--app-root".to_owned(),
        "/tmp/whisper-dictate".to_owned(),
    ]
}

struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(key);
        env::set_var(key, value);
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = env::var_os(key);
        env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            env::set_var(self.key, value);
        } else {
            env::remove_var(self.key);
        }
    }
}

#[test]
fn runtime_state_labels_are_stable() {
    assert_eq!(RuntimeState::Stopped.label(), "Stopped");
    assert_eq!(RuntimeState::Starting.label(), "Starting");
    assert_eq!(RuntimeState::Running.label(), "Running");
}

#[test]
fn worker_command_launches_python_directly() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");

    let root = PathBuf::from("/tmp/whisper-dictate");
    let command = worker_command(&root);

    assert_eq!(command.program, PathBuf::from(default_python_name()));
    assert_eq!(command.args, runtime_module_args());
    assert_eq!(command.working_dir, root);
    assert!(command.env.contains(&(
        PYTHONPATH_ENV.to_owned(),
        Path::new("/tmp/whisper-dictate")
            .join("src")
            .join("python")
            .display()
            .to_string()
    )));
}

#[test]
fn worker_command_appends_passthrough_args() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");

    let command = worker_command_with_args(
        "/tmp/whisper-dictate",
        ["--key".to_owned(), "shift_r+ctrl_r".to_owned()],
    );

    assert_eq!(
        command.args,
        vec![
            "-m".to_owned(),
            "whisper_dictate.runtime".to_owned(),
            "--app-root".to_owned(),
            "/tmp/whisper-dictate".to_owned(),
            "--key".to_owned(),
            "shift_r+ctrl_r".to_owned(),
        ]
    );
}

#[test]
fn worker_command_does_not_force_utf8_for_foreground_console() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");

    let command = worker_command("/tmp/whisper-dictate");

    assert!(!command.env.iter().any(|(key, _)| key == PYTHON_UTF8_ENV));
    assert!(!command
        .env
        .iter()
        .any(|(key, _)| key == PYTHON_IO_ENCODING_ENV));
}

#[test]
fn worker_command_honors_python_override() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = PathBuf::from("/tmp/whisper-dictate");

    let _python_guard = EnvVarGuard::set(PYTHON_ENV, "/custom/python");
    let command = worker_command(root);

    assert_eq!(command.program, PathBuf::from("/custom/python"));
}

#[test]
fn worker_command_prefers_existing_project_venv_python() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let python = if cfg!(windows) {
        dir.path()
            .join("voice-pi-venv")
            .join("Scripts")
            .join("python.exe")
    } else {
        dir.path()
            .join(".venv-whisper-dictate")
            .join("bin")
            .join("python")
    };
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(&python, "").unwrap();

    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _home_guard = EnvVarGuard::set("HOME", dir.path());
    let command = worker_command("/tmp/whisper-dictate");

    assert_eq!(command.program, python);
}

#[test]
fn default_worker_command_honors_app_root_override() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, "/installed/app");

    let command = default_worker_command();

    assert_eq!(command.working_dir, PathBuf::from("/installed/app"));
    assert_eq!(
        command.args,
        vec![
            "-m".to_owned(),
            "whisper_dictate.runtime".to_owned(),
            "--app-root".to_owned(),
            "/installed/app".to_owned(),
        ]
    );
}

#[test]
fn app_root_can_be_inferred_from_installed_exe_directory() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join(if cfg!(windows) {
        "whisper-dictate.exe"
    } else {
        "whisper-dictate"
    });
    let runtime = dir
        .path()
        .join("src")
        .join("python")
        .join("whisper_dictate")
        .join("runtime.py");
    std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
    std::fs::write(runtime, "").unwrap();

    assert_eq!(app_root_from_exe_path(&exe), Some(dir.path().to_path_buf()));
}

#[test]
fn version_prefers_version_file_without_v_prefix() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("VERSION"), "v9.8.7\n").unwrap();
    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, dir.path());

    assert_eq!(version(), "9.8.7");
}

#[cfg(windows)]
#[test]
fn stale_process_cleanup_script_is_scoped_to_current_exe_and_app_root() {
    let script = stale_process_cleanup_script(
        123,
        Path::new(r"C:\Program Files\WhisperDictate\whisper-dictate.exe"),
        Path::new(r"C:\Program Files\WhisperDictate"),
    );

    assert!(script.contains("$currentPid = 123"));
    assert!(script.contains("$cleanupPid = $PID"));
    assert!(script.contains(r"$_.ExecutablePath -eq $exe"));
    assert!(script.contains("$_.ProcessId -ne $cleanupPid"));
    assert!(script.contains(r#"$_.CommandLine -like "*whisper_dictate.runtime*""#));
    assert!(script.contains(r#"$_.CommandLine -like "*$root*""#));
    assert!(!script.contains("Stop-Process -Name python"));
    assert!(!script.contains("taskkill /IM python"));
}

#[cfg(windows)]
#[test]
fn powershell_single_quote_escape_doubles_quotes() {
    assert_eq!(
        escape_powershell_single_quoted(r"C:\It's\app"),
        r"C:\It''s\app"
    );
}

#[cfg(windows)]
#[test]
fn windows_shell_prefers_pwsh_when_present_on_path() {
    if env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|dir| dir.join("pwsh.exe").exists()))
        .unwrap_or(false)
    {
        assert_eq!(windows_shell_program(), "pwsh.exe");
    }
}

#[test]
fn doctor_command_adds_doctor_argument() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _app_root_guard = EnvVarGuard::set(APP_ROOT_ENV, "/installed/app");
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    let command = doctor_command();

    assert_eq!(
        command.args,
        vec![
            "-m".to_owned(),
            "whisper_dictate.runtime".to_owned(),
            "--app-root".to_owned(),
            "/installed/app".to_owned(),
            "--doctor".to_owned()
        ]
    );
}

#[test]
fn install_command_runs_rust_cli_from_app_root() {
    let command = install_command_from_exe("/installed/app/whisper-dictate", "/installed/app");

    assert_eq!(
        command.program,
        PathBuf::from("/installed/app/whisper-dictate")
    );
    assert_eq!(command.args, vec!["install".to_owned()]);
    assert_eq!(command.working_dir, PathBuf::from("/installed/app"));
}

#[test]
fn linux_desktop_entry_uses_supplied_absolute_exec_command() {
    let entry = linux_desktop_entry(
        false,
        "/opt/whisper-dictate/whisper-dictate ui",
        Path::new("/home/test/.local/share/icons/hicolor/scalable/apps/whisper-dictate.svg"),
    );

    assert!(entry.contains("Exec=/opt/whisper-dictate/whisper-dictate ui\n"));
    assert!(entry.contains(
        "Icon=/home/test/.local/share/icons/hicolor/scalable/apps/whisper-dictate.svg\n"
    ));
    assert!(entry.contains("StartupWMClass=whisper-dictate\n"));
    assert!(!entry.contains("Exec=whisper-dictate ui"));
    assert!(!entry.contains("X-GNOME-Autostart-enabled=true"));
}

#[test]
fn linux_autostart_entry_marks_gnome_autostart_enabled() {
    let entry = linux_desktop_entry(
        true,
        "/opt/whisper-dictate/whisper-dictate ui",
        Path::new("/home/test/.local/share/icons/hicolor/scalable/apps/whisper-dictate.svg"),
    );

    assert!(entry.contains("X-GNOME-Autostart-enabled=true"));
}

#[test]
fn desktop_exec_token_quotes_paths_with_spaces() {
    let token = desktop_exec_token(Path::new("/tmp/Whisper Dictate/whisper-dictate"));

    assert_eq!(token, "\"/tmp/Whisper Dictate/whisper-dictate\"");
}

#[test]
fn run_capture_returns_stdout_stderr_and_status() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let command = WorkerCommand {
        program: PathBuf::from("cmd.exe"),
        args: vec![
            "/C".to_owned(),
            "echo out line & echo err line 1>&2 & exit /B 7".to_owned(),
        ],
        working_dir: dir.path().to_path_buf(),
        env: Vec::new(),
    };
    #[cfg(not(windows))]
    let command = WorkerCommand {
        program: PathBuf::from("sh"),
        args: vec![
            "-c".to_owned(),
            "echo out line; echo err line >&2; exit 7".to_owned(),
        ],
        working_dir: dir.path().to_path_buf(),
        env: Vec::new(),
    };

    let output = run_capture(&command).unwrap();

    assert!(!output.success());
    assert_eq!(output.code(), Some(7));
    assert!(output.stdout.contains("out line"));
    assert!(output.stderr.contains("err line"));
}

#[test]
fn run_capture_preserves_utf8_danish_output_from_python() {
    let dir = tempfile::tempdir().unwrap();
    let command = WorkerCommand {
        program: PathBuf::from(default_python_name()),
        args: vec!["-c".to_owned(), "print('ændret prøv')".to_owned()],
        working_dir: dir.path().to_path_buf(),
        env: Vec::new(),
    };

    let output = run_capture(&command).unwrap();

    assert!(output.success());
    assert!(output.stdout.contains("ændret prøv"));
    assert!(!output.stdout.contains("Ã¦"));
    assert!(!output.stdout.contains("Ã¸"));
}

#[test]
fn install_commands_use_background_process_flags() {
    let runtime = include_str!("../runtime.rs");
    let run_install_command = runtime
        .split_once("fn run_install_command")
        .unwrap()
        .1
        .split_once("fn wants_parakeet_backend")
        .unwrap()
        .0;

    assert!(run_install_command.contains("configure_background_process(&mut process);"));
    assert!(!run_install_command.contains("Command::new(&command.program)\n        .args"));
}

#[test]
fn install_plan_uses_named_cpu_requirements_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("requirements")).unwrap();
    std::fs::write(dir.path().join("requirements").join("cpu.txt"), "").unwrap();
    let plan = InstallPlan::from_parts(
        dir.path().to_path_buf(),
        requirements_path(dir.path()).unwrap(),
        PathBuf::from("/venv/bin/python"),
        None,
    );

    assert_eq!(
        plan.requirements,
        dir.path().join("requirements").join("cpu.txt")
    );
    assert_eq!(
        plan.install_commands[1].args,
        vec![
            "-m",
            "pip",
            "install",
            "-r",
            plan.requirements.to_str().unwrap()
        ]
    );
}

#[test]
fn install_plan_accepts_legacy_requirements_file_from_old_linux_bundles() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("requirements.txt"), "").unwrap();

    assert_eq!(
        requirements_path(dir.path()).unwrap(),
        dir.path().join("requirements.txt")
    );
}

#[test]
fn install_plan_accepts_legacy_named_requirements_file_from_old_bundles() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("requirements-cpu.txt"), "").unwrap();

    assert_eq!(
        requirements_path(dir.path()).unwrap(),
        dir.path().join("requirements-cpu.txt")
    );
}

#[test]
fn install_plan_includes_parakeet_requirements_when_backend_requests_it() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("requirements")).unwrap();
    std::fs::write(dir.path().join("requirements").join("cpu.txt"), "").unwrap();
    std::fs::write(dir.path().join("requirements").join("parakeet.txt"), "").unwrap();

    let _python_guard = EnvVarGuard::set(PYTHON_ENV, "/custom/python");
    let _backend_guard = EnvVarGuard::set(STT_BACKEND_ENV, "parakeet");
    let plan = InstallPlan::for_current_environment(dir.path().to_path_buf()).unwrap();

    assert_eq!(plan.install_commands.len(), 3);
    assert_eq!(
        plan.install_commands[2].args[4],
        dir.path()
            .join("requirements")
            .join("parakeet.txt")
            .display()
            .to_string()
    );
}

#[test]
fn install_plan_includes_gpu_requirements_when_cuda_device_requests_it() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("requirements")).unwrap();
    std::fs::write(dir.path().join("requirements").join("cpu.txt"), "").unwrap();
    std::fs::write(dir.path().join("requirements").join("gpu.txt"), "").unwrap();

    let _python_guard = EnvVarGuard::set(PYTHON_ENV, "/custom/python");
    let _device_guard = EnvVarGuard::set("VOICEPI_DEVICE", "cuda");
    let plan = InstallPlan::for_current_environment(dir.path().to_path_buf()).unwrap();

    assert_eq!(plan.install_commands.len(), 3);
    assert_eq!(
        plan.install_commands[2].args[4],
        dir.path()
            .join("requirements")
            .join("gpu.txt")
            .display()
            .to_string()
    );
}

#[test]
fn install_plan_skips_missing_parakeet_requirements() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("requirements")).unwrap();
    std::fs::write(dir.path().join("requirements").join("cpu.txt"), "").unwrap();

    let _python_guard = EnvVarGuard::set(PYTHON_ENV, "/custom/python");
    let _backend_guard = EnvVarGuard::set(STT_BACKEND_ENV, "parakeet");
    let plan = InstallPlan::for_current_environment(dir.path().to_path_buf()).unwrap();

    assert_eq!(plan.install_commands.len(), 2);
}

#[test]
fn venv_paths_match_platform_conventions() {
    let home = PathBuf::from("/home/person");
    assert_eq!(
        venv_python_path(&default_venv_dir(&home, Platform::Unix), Platform::Unix),
        PathBuf::from("/home/person/.venv-whisper-dictate/bin/python")
    );

    let home = PathBuf::from("C:/Users/Person");
    assert_eq!(
        venv_python_path(
            &default_venv_dir(&home, Platform::Windows),
            Platform::Windows
        ),
        PathBuf::from("C:/Users/Person/voice-pi-venv/Scripts/python.exe")
    );
}

#[test]
fn parses_worker_event_lines() {
    let event = parse_worker_event(
        r#"[worker-event] {"event":"status","state":"ready","model":"large-v3"}"#,
    )
    .unwrap();

    assert_eq!(event.event, "status");
    assert_eq!(event.state.as_deref(), Some("ready"));
    assert_eq!(event.payload["model"], "large-v3");
}

#[test]
fn invalid_worker_event_lines_fall_back_to_stderr() {
    assert!(parse_worker_event("[worker-event] not json").is_none());
    assert!(parse_worker_event("ordinary stderr").is_none());
}
