//! The public GUI logger must use the bounded writer, not an unbounded file.

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn installed_gui_logger_rotates_complete_records_and_keeps_one_recovery_generation() {
    use std::fs;
    use whisper_dictate_app::diag;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    let backup = dir.path().join("gui.log.previous");
    let config = dir.path().join("config.json");
    fs::write(&config, b"{}").unwrap();
    // Integration tests have their own process; no library env lock is shared.
    std::env::set_var("VOICEPI_CONFIG", config);
    std::env::remove_var("VOICEPI_HISTORY_JSONL");
    std::env::remove_var("VOICEPI_METRICS_JSONL");
    std::env::set_var("VOICEPI_LOG", "info");
    diag::init_from_env();

    let limit = 4 * 1024 * 1024;
    let original = b"prior\n".repeat(limit / 6);
    fs::write(&path, &original).unwrap();
    diag::install_gui_diagnostic_log(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!backup.exists(), "installation alone must not rotate");

    diag::write_line("[test] first complete record");
    let active = fs::read_to_string(&path).unwrap();
    assert!(active.contains("[test] first complete record"));
    assert!(active.ends_with('\n'));
    assert!(!active.contains("prior"));
    assert!(active.len() <= limit);
    assert_eq!(fs::read(&backup).unwrap(), original);

    diag::write_line("[test] second complete record");
    let active = fs::read_to_string(&path).unwrap();
    assert!(active.contains("[test] first complete record"));
    assert!(active.contains("[test] second complete record"));
    assert!(active.len() <= limit);
    assert_eq!(fs::read(backup).unwrap(), original);
}
