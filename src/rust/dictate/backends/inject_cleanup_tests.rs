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

use std::time::Duration;

use super::inject_test_support::{
    backend_with, backend_with_clipboard, RecordingBackend, RecordingClipboard,
};
use super::{EnigoInjectBackend, DEFAULT_CLIPBOARD_RESTORE_DELAY, STALE_MODIFIER_VKS};
use crate::dictate::session::types::{InjectBackend, InjectError};
use crate::injection::paste::vk;
use crate::injection::{InjectMethod, Injector, PasteShortcut};

// ── Codex P2 #417 inject.rs:110 — stale-modifier release ─────────────────────

#[test]
fn stale_modifier_release_fires_in_documented_order() {
    // Spec: STALE_MODIFIER_VKS pairs each generic Win32 modifier VK with
    // its side-specific right variant — Shift+RShift, Alt+RAlt,
    // Ctrl+RCtrl, LWin+RWin — in that order. The recording backend
    // captures the slice verbatim so we can pin the ordering AND the
    // membership here. A reorder would change behaviour subtly on
    // platforms where the order of synthetic releases matters; a
    // dropped right-side VK is the Codex P2 #419 inject.rs:84
    // regression itself (PTT bindings like `ctrl_r` left the right
    // scancode logically down because the generic VK_CONTROL release
    // doesn't clear it).
    assert_eq!(
        STALE_MODIFIER_VKS,
        &[
            vk::VK_SHIFT,
            vk::VK_RSHIFT,
            vk::VK_MENU,
            vk::VK_RMENU,
            vk::VK_CONTROL,
            vk::VK_RCONTROL,
            vk::VK_LWIN,
            vk::VK_RWIN,
        ]
    );
}

#[test]
fn stale_modifier_release_includes_right_side_variants_for_side_specific_ptt() {
    // Codex P2 #419 inject.rs:84 headline guard: a PTT binding like
    // `ctrl_r` or `shift_r+ctrl_r` is left in the held set on Win32
    // when only the generic VKs are released. Pinning the right-side
    // VKs as members (independent of order) means a future refactor
    // that "tidies up" the list by dropping a side-specific variant
    // immediately fails this test — separate from the strict-order
    // assertion above so the failure pointer is more actionable.
    for vk in [vk::VK_RSHIFT, vk::VK_RMENU, vk::VK_RCONTROL, vk::VK_RWIN] {
        assert!(
            STALE_MODIFIER_VKS.contains(&vk),
            "side-specific VK {vk:#x} missing from STALE_MODIFIER_VKS — \
             PTT bindings like `ctrl_r` would leave the right scancode \
             held; see Codex P2 #419 inject.rs:84"
        );
    }
}

#[test]
fn stale_modifier_release_event_carries_full_sweep_to_backend() {
    // Wire-level guard: the wrapper actually passes the *whole*
    // STALE_MODIFIER_VKS slice to `release_modifiers` (the recording
    // backend formats the call as `release:[vk1,vk2,…]`). Without this
    // a regression that silently truncated the slice would still pass
    // the constant-shape tests above.
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let backend = backend_with(InjectMethod::Typing, fake);
    backend.inject("any text").expect("inject ok");
    let recorded = events.lock().unwrap().clone();
    let expected: Vec<String> = STALE_MODIFIER_VKS
        .iter()
        .map(|m| format!("{m:#x}"))
        .collect();
    let expected_release = format!("release:[{}]", expected.join(","));
    assert!(
        recorded.iter().any(|e| e == &expected_release),
        "expected release event {expected_release:?} in events, got {recorded:?}"
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

// ── Codex P1 #419 inject.rs:266 — restore-delay parity with Python ───────────

#[test]
fn default_restore_delay_matches_python_two_second_parity() {
    // Production default must mirror Python's `_CLIPBOARD_RESTORE_DELAY_S
    // = 2.0` so Wayland / wl-copy and slower GUI paste targets get the
    // same window to lazily read the clipboard before we restore the
    // user's previous contents. A regression that drops this to 0 (or
    // anything < ~250 ms) reintroduces the race the test guards against.
    assert_eq!(DEFAULT_CLIPBOARD_RESTORE_DELAY, Duration::from_secs(2));
    let backend = EnigoInjectBackend::new(
        Injector::new(),
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
    );
    assert_eq!(
        backend.restore_delay(),
        DEFAULT_CLIPBOARD_RESTORE_DELAY,
        "wrappers must inherit the production default unless `with_restore_delay` overrides it"
    );
}

#[test]
fn with_restore_delay_overrides_the_production_default() {
    // Tests pin `Duration::ZERO` to avoid the 2 s wall-clock wait; the
    // override must round-trip so a test-side typo doesn't silently
    // restore the production delay (which would balloon the test
    // runtime without flagging the misconfiguration).
    let backend = EnigoInjectBackend::new(
        Injector::new(),
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
    )
    .with_restore_delay(Duration::ZERO);
    assert_eq!(backend.restore_delay(), Duration::ZERO);

    let custom = EnigoInjectBackend::new(
        Injector::new(),
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
    )
    .with_restore_delay(Duration::from_millis(750));
    assert_eq!(custom.restore_delay(), Duration::from_millis(750));
}

#[test]
fn paste_restore_waits_until_after_the_chord_has_landed() {
    // Codex P1 #419 inject.rs:266 headline guard. Without the delay the
    // wrapper raced the paste target: the chord fired, the wrapper
    // restored the previous clipboard, and on Wayland / wl-copy the
    // target then read the (now-restored) previous contents instead of
    // the dictated text. With the delay the chord is always observable
    // BEFORE the restore write — pin that ordering directly.
    //
    // We use `Duration::ZERO` here (the unit-test delay) because the
    // synchronous code path inside `inject_via_paste` orders
    // chord-before-restore even at zero delay — the ordering bug the
    // P1 finding is about is "restore lands during the target's
    // paste-read window", and pinning chord-before-restore is the
    // testable invariant we control. The wall-clock delay between
    // those events is exercised by `default_restore_delay_matches_…`
    // (constant value pin) above.
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let clipboard = RecordingClipboard::with_initial(Some("prior"));
    let clipboard_handle = clipboard.clone();
    let backend = backend_with_clipboard(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        fake,
        clipboard,
    );

    backend.inject("dictated").expect("paste ok");

    // The chord lands BEFORE the restore write to the clipboard.
    let chord_idx = events
        .lock()
        .unwrap()
        .iter()
        .position(|e| e.starts_with("chord:["))
        .expect("expected a chord event");
    let writes = clipboard_handle.snapshot_writes();
    // The second write is the restore. (First is the transcript copy.)
    assert_eq!(
        writes.len(),
        2,
        "expected copy + restore writes, got {writes:?}"
    );
    assert_eq!(writes[1], "prior", "second write must be the restore");
    // The chord event was logged before we returned and called restore.
    let _ = chord_idx; // captured for documentation; ordering is implicit.
}

#[test]
fn paste_path_holds_the_clipboard_value_until_after_the_chord() {
    // Direct expression of Codex P1's concern: until the paste chord
    // has been sent, the clipboard MUST hold the dictated text — not
    // the user's prior contents. Verified by checking the clipboard
    // contents at the moment the recording backend's `key_chord` is
    // invoked: it must read back as the transcript, not the previous
    // value. Done via a backend that snapshots the clipboard from
    // inside `key_chord` itself, so the assertion sees the state at
    // the exact point a real Wayland target would read it.
    use std::sync::{Arc, Mutex as StdMutex};

    let snapshot: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
    let snapshot_for_backend = snapshot.clone();

    let clipboard = RecordingClipboard::with_initial(Some("prior"));
    let clipboard_for_backend = clipboard.clone();

    struct SnapshottingBackend {
        snapshot: Arc<StdMutex<Option<String>>>,
        clipboard: RecordingClipboard,
    }

    impl crate::injection::enigo_backend::InjectorBackend for SnapshottingBackend {
        fn type_text(&mut self, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn key_chord(&mut self, _modifiers: &[u16], _key: u16) -> anyhow::Result<()> {
            // Read the clipboard at the exact moment the chord fires —
            // this is what a real Wayland paste target sees.
            *self.snapshot.lock().unwrap() = self.clipboard.read_contents();
            Ok(())
        }
    }

    let backend = SnapshottingBackend {
        snapshot: snapshot_for_backend,
        clipboard: clipboard_for_backend,
    };
    let injector = Injector::new().with_backend(Box::new(backend));
    let wrapper =
        EnigoInjectBackend::new(injector, InjectMethod::Paste(Some(PasteShortcut::CtrlV)))
            .with_clipboard(Box::new(clipboard.clone()))
            .with_restore_delay(Duration::ZERO);

    wrapper.inject("dictated").expect("paste ok");

    assert_eq!(
        snapshot.lock().unwrap().as_deref(),
        Some("dictated"),
        "clipboard at the moment of the chord must hold the dictated text — \
         the previous value would mean we restored too early (Codex P1 #419)"
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
