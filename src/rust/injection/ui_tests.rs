use super::{auto_method_for, resolve_method};
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
