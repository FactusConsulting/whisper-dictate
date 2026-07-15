use super::*;

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
