//! Codex P1 + P2 pre-injection-cleanup tests for `super::EnigoInjectBackend`.
//!
//! Covers the two side-channel concerns the wrapper closes BEFORE
//! delegating to `Injector::inject_text`:
//!
//! - **P2 #417 inject.rs:110** — stale modifiers (Shift / Alt /
//!   Ctrl / Cmd) are released in the documented order, and the
//!   release fires strictly before the type / chord event.
//! - **P1 #417 inject.rs:110** — paste mode writes the transcript
//!   to the configured clipboard, sends the chord, then restores the
//!   previous value via `PasteGuard`; paste without a configured
//!   clipboard surfaces a clear `InjectError::Backend` rather than
//!   silently pasting stale data.
//!
//! Delegation / error-mapping tests live in the sibling
//! `inject_tests` module. Shared recording fakes live in
//! `inject_test_support`.

use super::inject_test_support::{
    backend_with, backend_with_clipboard, RecordingBackend, RecordingClipboard,
};
use super::{EnigoInjectBackend, STALE_MODIFIER_VKS};
use crate::dictate::session::types::{InjectBackend, InjectError};
use crate::injection::paste::vk;
use crate::injection::{InjectMethod, Injector, PasteShortcut};

// ── Codex P2 #417 inject.rs:110 — stale-modifier release ─────────────────────

#[test]
fn stale_modifier_release_fires_in_documented_order() {
    // Spec: STALE_MODIFIER_VKS lists Shift, Alt (VK_MENU), Ctrl, Cmd
    // (VK_LWIN) in that order. The release event the recording backend
    // captures preserves the slice, so we can pin the exact ordering
    // here — a reorder would change behaviour subtly on platforms
    // where the order of synthetic releases matters and a test like
    // this is the cheapest place to catch it.
    assert_eq!(
        STALE_MODIFIER_VKS,
        &[vk::VK_SHIFT, vk::VK_MENU, vk::VK_CONTROL, vk::VK_LWIN]
    );
}

#[test]
fn stale_modifier_release_runs_before_the_injected_action() {
    // Drive a typing inject and assert the release event lands
    // strictly before the type event. Without the wrapper's
    // pre-injection sweep, a held Ctrl from a PTT chord would make
    // the typed burst land as `Ctrl+<char>` shortcuts — see
    // vp_inject.py::_release_stale_modifiers for the Python original.
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let backend = backend_with(InjectMethod::Typing, fake);
    backend.inject("payload").expect("inject ok");
    let recorded = events.lock().unwrap().clone();
    let release_idx = recorded
        .iter()
        .position(|e| e.starts_with("release:["))
        .expect("expected a release event");
    let type_idx = recorded
        .iter()
        .position(|e| e == "type:payload")
        .expect("expected a type event");
    assert!(
        release_idx < type_idx,
        "release must come before type: {recorded:?}"
    );
}

// ── Codex P1 #417 inject.rs:110 — paste owns the clipboard ───────────────────

#[test]
fn paste_writes_transcript_to_clipboard_then_restores_previous_value() {
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("user's prior copy"));
    let clipboard_handle = clipboard.clone();
    let backend = backend_with_clipboard(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        fake,
        clipboard,
    );

    backend
        .inject("dictated transcript")
        .expect("paste should succeed when a clipboard is configured");

    // The wrapper wrote the transcript (so the chord pastes the right
    // text) then restored the previous value via PasteGuard.
    let writes = clipboard_handle.snapshot_writes();
    assert_eq!(
        writes,
        vec![
            "dictated transcript".to_owned(),
            "user's prior copy".to_owned()
        ],
        "expected write(transcript) then write(previous), got {writes:?}"
    );
    assert_eq!(
        clipboard_handle.read_contents().as_deref(),
        Some("user's prior copy"),
        "previous clipboard value must be restored"
    );
    // PasteGuard reads once to stash the previous value, and once more
    // during restore to make sure the clipboard still holds OUR text
    // (so a mid-paste user copy isn't clobbered). Pin the count so a
    // regression that double-reads or stops verifying gets flagged.
    assert_eq!(clipboard_handle.read_count(), 2);
}

#[test]
fn paste_without_clipboard_surfaces_a_clear_backend_error() {
    // Without `with_clipboard`, the wrapper has no way to populate the
    // clipboard before sending the chord — silently sending Ctrl+V
    // would paste whatever the user already had on the clipboard
    // instead of the transcript. The wrapper must refuse loudly.
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let injector = Injector::new().with_backend(Box::new(fake));
    let backend =
        EnigoInjectBackend::new(injector, InjectMethod::Paste(Some(PasteShortcut::CtrlV)));

    let err = backend
        .inject("payload")
        .expect_err("paste without clipboard must error");
    match err {
        InjectError::Backend(msg) => {
            assert!(
                msg.contains("clipboard"),
                "error should mention the clipboard, got: {msg}"
            );
            assert!(
                msg.contains("with_clipboard"),
                "error should point at the builder, got: {msg}"
            );
        }
    }

    // Critically: no chord was sent. The release fires first (the
    // wrapper sweeps before checking the paste prerequisites), but no
    // `chord:` event must be in the log — otherwise we'd have pasted
    // stale clipboard contents.
    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded.iter().all(|e| !e.starts_with("chord:")),
        "no chord must be sent when paste pre-conditions fail: {recorded:?}"
    );
}

#[test]
fn paste_aborts_before_chord_when_clipboard_write_fails() {
    // A clipboard backend that refuses writes must abort the inject
    // BEFORE the chord fires — otherwise we'd paste stale data even
    // though the clipboard is configured. Same surface area as the
    // "no clipboard" path, but a different failure mode (the OS
    // clipboard rejecting our write, e.g. another app holding the
    // selection).
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let clipboard = RecordingClipboard::with_initial(Some("prior"));
    clipboard.arm_write_failure();
    let clipboard_handle = clipboard.clone();
    let backend = backend_with_clipboard(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        fake,
        clipboard,
    );

    let err = backend
        .inject("never pasted")
        .expect_err("clipboard write failure must abort the inject");
    match err {
        InjectError::Backend(msg) => {
            assert!(
                msg.contains("clipboard write failed"),
                "error must explain the failure, got: {msg}"
            );
        }
    }

    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded.iter().all(|e| !e.starts_with("chord:")),
        "chord must not fire when the clipboard write fails: {recorded:?}"
    );
    // The clipboard contents must be unchanged.
    assert_eq!(clipboard_handle.read_contents().as_deref(), Some("prior"));
}

#[test]
fn paste_chord_failure_still_restores_previous_clipboard() {
    // If enigo / the helper chain fails mid-chord, the paste guard
    // must still restore the previous clipboard contents — otherwise
    // a transient injection error would leave the dictated text
    // sitting on the user's clipboard.
    let fake = RecordingBackend::failing("simulated enigo failure");
    let clipboard = RecordingClipboard::with_initial(Some("prior"));
    let clipboard_handle = clipboard.clone();
    let backend = backend_with_clipboard(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        fake,
        clipboard,
    );

    let err = backend
        .inject("dictated")
        .expect_err("armed failure should bubble up");
    assert!(matches!(err, InjectError::Backend(_)));

    // The previous clipboard contents are restored even though the
    // chord failed — the wrapper's restore call runs irrespective of
    // the inject result.
    assert_eq!(
        clipboard_handle.read_contents().as_deref(),
        Some("prior"),
        "previous clipboard must be restored on chord failure too"
    );
}

#[test]
fn typing_path_ignores_clipboard_entirely() {
    // Typing mode has no clipboard dependency — the wrapper must not
    // read / write the clipboard even when one is configured. Pin
    // that behaviour so a future refactor that accidentally routes
    // typing through the paste arm is caught.
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("untouched"));
    let clipboard_handle = clipboard.clone();
    let backend = backend_with_clipboard(InjectMethod::Typing, fake, clipboard);
    backend.inject("typed text").expect("typing inject ok");

    assert!(
        clipboard_handle.snapshot_writes().is_empty(),
        "typing path must not write to the clipboard"
    );
    assert_eq!(clipboard_handle.read_count(), 0);
}
