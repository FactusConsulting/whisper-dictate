use super::{
    auto_method_for, dedup_by_restore_coordinator, is_text_clipboard_format, resolve_method,
    should_fallback_auto_paste,
};
use crate::dictate::backends::EnigoInjectBackend;
use crate::dictate::session::types::InjectError;
use crate::injection::{InjectMethod, Injector, LinuxSession};
use std::sync::Arc;

#[test]
fn coordinated_backends_deduplicate_shared_restore_coordinators() {
    // — paste-last adopts the session's
    // restore coordinator, so a session backend and an ephemeral backend
    // can hold the same non-reentrant restore mutex. The deduplication
    // must yield each coordinator exactly once, or the per-entry
    // with_restore_guard acquisition deadlocks on the same thread.
    let session = Arc::new(EnigoInjectBackend::new(
        Injector::new(),
        InjectMethod::Paste(None),
    ));
    let ephemeral = Arc::new(
        EnigoInjectBackend::new(Injector::new(), InjectMethod::Typing)
            .with_restore_handle(session.restore_handle()),
    );
    let unshared = Arc::new(EnigoInjectBackend::new(
        Injector::new(),
        InjectMethod::Typing,
    ));
    let mut all = vec![
        Arc::clone(&session),
        Arc::clone(&ephemeral),
        Arc::clone(&unshared),
    ];
    dedup_by_restore_coordinator(&mut all);
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|b| Arc::ptr_eq(b, &unshared)));
}

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

#[test]
fn auto_paste_fallback_is_limited_to_safe_failures() {
    let error = InjectError::Backend("no Linux paste helper available".to_owned());
    assert!(should_fallback_auto_paste(
        "auto",
        InjectMethod::Paste(None),
        &error,
        "linux"
    ));
    assert!(!should_fallback_auto_paste(
        "paste",
        InjectMethod::Paste(None),
        &error,
        "linux"
    ));
    // Other failures stay gated off Windows; only a refused clipboard
    // write unlocks the typing fallback there (rich-selection refusals,
    // where nothing was typed and no chord was sent).
    assert!(!should_fallback_auto_paste(
        "auto",
        InjectMethod::Paste(None),
        &error,
        "windows"
    ));
    let refused = InjectError::Backend(
        "clipboard write failed; refusing to send paste shortcut \
         against stale clipboard contents"
            .to_owned(),
    );
    assert!(should_fallback_auto_paste(
        "auto",
        InjectMethod::Paste(None),
        &refused,
        "windows"
    ));
    // An explicit paste request never silently degrades to typing.
    assert!(!should_fallback_auto_paste(
        "paste",
        InjectMethod::Paste(None),
        &refused,
        "windows"
    ));
    // A typing request has nothing to fall back from.
    assert!(!should_fallback_auto_paste(
        "auto",
        InjectMethod::Typing,
        &refused,
        "windows"
    ));
}
