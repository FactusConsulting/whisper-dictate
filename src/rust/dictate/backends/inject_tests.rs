//! Delegation + error-mapping tests for [`super::EnigoInjectBackend`].
//!
//! Covers the trait-impl surface: typing/paste flows reach the
//! underlying backend with the correct method, a backend failure
//! surfaces as `InjectError::Backend(_)`, and the configured
//! `InjectMethod` round-trips.
//!
//! The Codex P1 + P2 pre-injection-cleanup behaviour (clipboard
//! ownership, stale-modifier release) lives in the sibling
//! `inject_cleanup_tests` module so each file stays under the repo's
//! ~500-line modularity gate. Shared recording fakes live in
//! `inject_test_support`.

use super::inject_test_support::{
    backend_with, backend_with_clipboard, RecordingBackend, RecordingClipboard,
};
use crate::dictate::session::types::{InjectBackend, InjectError};
use crate::injection::{InjectMethod, PasteShortcut};

#[test]
fn inject_typing_delegates_text_to_underlying_backend() {
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let backend = backend_with(InjectMethod::Typing, fake);
    backend.inject("hello world").expect("inject ok");
    let recorded = events.lock().unwrap().clone();
    // The wrapper always sweeps held modifiers before the action, so
    // the recorded events include a leading `release:` line — assert
    // the type event is present without pinning the exact order /
    // count of release events (that's the cleanup-tests scope).
    assert!(
        recorded.iter().any(|e| e == "type:hello world"),
        "expected type:hello world in events, got {recorded:?}"
    );
}

#[test]
fn inject_paste_with_explicit_shortcut_emits_chord() {
    // `Some(CtrlV)` is the simplest paste path — verify we reach
    // `key_chord` (no double-press / split into multiple chords) and
    // that the text isn't typed at all on the paste path. The actual
    // VK codes are an injection-module concern; we just confirm we
    // get exactly one chord event.
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let clipboard = RecordingClipboard::with_initial(Some("prior"));
    let backend = backend_with_clipboard(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        fake,
        clipboard,
    );
    backend.inject("ignored on paste path").expect("inject ok");
    let recorded = events.lock().unwrap().clone();
    let chords: Vec<&String> = recorded
        .iter()
        .filter(|e| e.starts_with("chord:["))
        .collect();
    assert_eq!(
        chords.len(),
        1,
        "expected exactly one chord event, got {recorded:?}"
    );
}

#[test]
fn inject_failure_is_wrapped_in_backend_error() {
    let fake = RecordingBackend::failing("xtest could not reach display");
    let backend = backend_with(InjectMethod::Typing, fake);
    let err = backend
        .inject("nope")
        .expect_err("recording backend was armed to fail");
    match err {
        InjectError::Backend(msg) => {
            assert!(
                msg.contains("xtest could not reach display"),
                "expected wrapped backend error message, got: {msg}"
            );
        }
    }
}

#[test]
fn method_accessor_round_trips() {
    let backend = backend_with(
        InjectMethod::Paste(Some(PasteShortcut::CtrlShiftV)),
        RecordingBackend::new(),
    );
    assert_eq!(
        backend.method(),
        InjectMethod::Paste(Some(PasteShortcut::CtrlShiftV))
    );
}

#[test]
fn paste_none_falls_through_to_default_chord() {
    // `Paste(None)` means "no explicit shortcut, dispatcher picks at
    // dispatch time". On the trait-backend path (Win/macOS, which the
    // test injection hits regardless of platform) that collapses to
    // `PasteShortcut::default()`. Verify we still emit a chord rather
    // than swallowing the call.
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let clipboard = RecordingClipboard::with_initial(Some("prior"));
    let backend = backend_with_clipboard(InjectMethod::Paste(None), fake, clipboard);
    backend.inject("anything").expect("inject ok");
    let recorded = events.lock().unwrap().clone();
    let chords: Vec<&String> = recorded
        .iter()
        .filter(|e| e.starts_with("chord:["))
        .collect();
    assert_eq!(
        chords.len(),
        1,
        "expected exactly one chord event for Paste(None), got {recorded:?}"
    );
}

#[test]
fn injection_method_can_be_invoked_multiple_times() {
    // `EnigoInjectBackend::inject` takes `&self`, so the supervisor
    // can call it once per utterance without re-constructing. The
    // interior `Mutex` must serialise correctly — drive two
    // back-to-back calls and confirm both events landed in order.
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let backend = backend_with(InjectMethod::Typing, fake);
    backend.inject("first").unwrap();
    backend.inject("second").unwrap();
    let recorded = events.lock().unwrap().clone();
    let types: Vec<&String> = recorded.iter().filter(|e| e.starts_with("type:")).collect();
    assert_eq!(
        types,
        vec![&"type:first".to_owned(), &"type:second".to_owned()],
        "expected ordered type events, got {recorded:?}"
    );
}
