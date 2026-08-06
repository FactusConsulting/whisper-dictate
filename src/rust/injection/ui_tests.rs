use super::{auto_method_for, is_text_clipboard_format, resolve_method};
use crate::injection::{InjectMethod, LinuxSession};

#[test]
fn explicit_ui_modes_are_preserved() {
    assert_eq!(
        resolve_method("type", "hello").unwrap(),
        InjectMethod::Typing
    );
    assert_eq!(
        resolve_method("paste", "hello").unwrap(),
        InjectMethod::Paste(None)
    );
    assert!(resolve_method("print", "hello").is_err());
}

#[test]
fn auto_ui_mode_uses_paste_for_non_ascii_wayland_text() {
    assert_eq!(
        auto_method_for("æøå", "linux", LinuxSession::OtherWayland),
        InjectMethod::Paste(None)
    );
    assert_eq!(
        auto_method_for("hello", "linux", LinuxSession::OtherWayland),
        InjectMethod::Typing
    );
    assert_eq!(
        auto_method_for("hello", "windows", LinuxSession::Unknown),
        InjectMethod::Paste(None)
    );
}

#[test]
fn clipboard_backup_accepts_only_plain_text_formats() {
    assert!(is_text_clipboard_format(1));
    assert!(is_text_clipboard_format(7));
    assert!(is_text_clipboard_format(13));
    assert!(is_text_clipboard_format(16));
    assert!(!is_text_clipboard_format(49324));
}
