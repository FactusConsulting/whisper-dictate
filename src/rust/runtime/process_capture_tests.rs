use super::*;

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
fn decode_capped_output_keeps_utf8_tail_with_marker() {
    let prefix = "x".repeat(CAPTURE_OUTPUT_MAX_CHARS + 10);
    let raw = format!("{prefix}æøå");
    let out = decode_capped_output(raw.as_bytes());

    assert!(out.starts_with("[ui] ...older captured output trimmed..."));
    assert!(out.ends_with("æøå"));
    assert!(out.len() <= CAPTURE_OUTPUT_MAX_CHARS + 64);
}

#[test]
fn install_commands_use_background_process_flags() {
    let runtime = include_str!("../runtime.rs");
    // After Wave 8 of #348 removed `wants_parakeet_backend`, the next
    // function below `run_install_command` is `wants_cuda_runtime`.
    let run_install_command = runtime
        .split_once("fn run_install_command")
        .unwrap()
        .1
        .split_once("fn wants_cuda_runtime")
        .unwrap()
        .0;

    assert!(run_install_command.contains("configure_background_process(&mut process);"));
    assert!(!run_install_command.contains("Command::new(&command.program)\n        .args"));
}

#[test]
fn windows_taskkill_stop_uses_background_process_flags() {
    let runtime = include_str!("../runtime.rs");
    let kill_child = runtime
        .split_once("fn kill_child")
        .unwrap()
        .1
        .split_once("fn python_program")
        .unwrap()
        .0;

    assert!(kill_child.contains("Command::new(\"taskkill\")"));
    assert!(kill_child.contains("configure_background_process(&mut command);"));
    assert!(kill_child.contains(".stdout(Stdio::null())"));
    assert!(kill_child.contains(".stderr(Stdio::null())"));
}
