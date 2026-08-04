use super::hotkey::{hotkey_warning, HotkeyWarning};

#[test]
fn pause_is_not_reported_as_unsupported() {
    assert_eq!(hotkey_warning("pause"), None);
}

#[test]
fn non_native_named_keys_are_reported() {
    assert_eq!(
        hotkey_warning("insert"),
        Some(HotkeyWarning::UnsupportedToken("insert".to_owned()))
    );
}
