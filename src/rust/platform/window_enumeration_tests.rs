//! Regression coverage for native visible-window enumeration.

use super::*;

#[test]
fn filters_cli_and_gui_process_names_case_insensitively() {
    assert!(is_self_window("Settings", Some("whisper-dictate.exe")));
    assert!(is_self_window(
        "Settings",
        Some(r"C:\Apps\Whisper-Dictate-GUI.EXE")
    ));
    assert!(is_self_window("Settings", Some("whisper_dictate")));
    assert!(!is_self_window("Settings", Some("notepad.exe")));
    assert!(!is_self_window("Settings", None));
}

#[test]
fn filters_self_titles_with_optional_numeric_version() {
    assert!(is_self_window("Whisper-Dictate", None));
    assert!(is_self_window("  WHISPER-DICTATE   1.22.4  ", None));
    assert!(!is_self_window("Whisper-Dictate Settings", None));
    assert!(!is_self_window("Whisper-Dictate beta", None));
}

#[test]
fn process_name_uses_windows_basename_when_query_succeeds() {
    assert_eq!(
        process_name_or_pid(42, Some(r"C:\Program Files\Notepad\notepad.exe")),
        "notepad.exe"
    );
    assert_eq!(
        process_name_or_pid(42, Some("C:/Tools/Code.exe")),
        "Code.exe"
    );
}

#[test]
fn process_name_falls_back_to_pid_when_image_query_fails() {
    assert_eq!(process_name_or_pid(4242, None), "4242");
}

#[cfg(not(target_os = "windows"))]
#[test]
fn enumeration_reports_unsupported_platform() {
    assert_eq!(
        list_visible_windows().unwrap_err(),
        "window listing is only supported on Windows"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn live_enumeration_never_returns_blank_or_self_windows() {
    for window in list_visible_windows().unwrap() {
        assert!(!window.title.trim().is_empty());
        assert!(!is_self_window(&window.title, Some(&window.process)));
    }
}
