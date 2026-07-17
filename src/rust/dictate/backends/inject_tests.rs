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

use std::sync::Arc;

use super::inject_test_support::{
    backend_with, backend_with_clipboard, RecordingBackend, RecordingClipboard,
};
use crate::dictate::session::types::{InjectBackend, InjectError};
use crate::hotkey::InjectionGuard;
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

// -----------------------------------------------------------------------
// Self-injection guard bracket integration (Windows PTT wedge — re-land
// of PR #476 with the bracket-pattern fix for long bursts). Verifies the
// wrapper actually raises the shared guard around the SendInput bursts
// so the hotkey listener drops them. See
// `crate::hotkey::inject_guard` for the timing model and the standalone
// unit tests of the guard itself.
// -----------------------------------------------------------------------

/// The counter-based bracket must be open while the injector is
/// running. Uses a recording backend that observes the guard state
/// mid-`type_text` — if the wrapper armed the guard AFTER the
/// SendInput (or not at all), the recorded state would be `inactive`
/// and the assertion below would fail. This is the load-bearing
/// invariant: an inject that leaves the guard inactive during the
/// burst leaks synthesised events into the PTT tracker on Windows.
#[test]
fn inject_arms_shared_guard_during_the_send_burst() {
    use crate::injection::enigo_backend::InjectorBackend;
    use crate::injection::Injector;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Recording backend that captures `is_active` on the shared
    /// guard AT the moment its `type_text` is called. This is the
    /// closest we can get to "was the guard raised at the exact
    /// moment SendInput would have fired" in a unit test without
    /// spawning an rdev listener.
    struct GuardObservingBackend {
        guard: Arc<InjectionGuard>,
        observed_active: Arc<Mutex<Option<bool>>>,
    }
    impl InjectorBackend for GuardObservingBackend {
        fn type_text(&mut self, _text: &str) -> Result<(), anyhow::Error> {
            *self.observed_active.lock().unwrap() = Some(self.guard.is_active());
            Ok(())
        }
        fn key_chord(&mut self, _modifiers: &[u16], _key: u16) -> Result<(), anyhow::Error> {
            // Unused by this test (typing method only), but the trait
            // is not defaulted here so we implement a no-op.
            Ok(())
        }
    }

    let guard = Arc::new(InjectionGuard::new());
    let observed = Arc::new(Mutex::new(None));
    let backend = GuardObservingBackend {
        guard: Arc::clone(&guard),
        observed_active: Arc::clone(&observed),
    };
    let wrapper = super::EnigoInjectBackend::new(
        Injector::new().with_backend(Box::new(backend)),
        InjectMethod::Typing,
    )
    .with_restore_delay(Duration::ZERO)
    .with_injection_guard(Arc::clone(&guard));

    wrapper.inject("guarded").expect("typing inject");

    let seen = observed.lock().unwrap();
    assert_eq!(
        *seen,
        Some(true),
        "guard must be active during the SendInput burst; if this fails, \
         the bracket is not being opened around inject_text (Windows PTT wedge \
         will recur — see crate::hotkey::inject_guard)"
    );

    // And after inject() returns, the counter-based bracket has been
    // closed, so `is_active` depends purely on the post-arm horizon.
    // 200 ms POST_GRACE means the guard is still active immediately
    // after return — a small waited window then decays.
    assert!(
        guard.is_active(),
        "post-arm grace horizon must still cover us immediately after inject returns"
    );
}

/// If no guard is installed AND no process-wide slot is populated,
/// the wrapper is a no-op around the shared guard — the arm becomes a
/// silent skip and the existing (pre-#476) delegation behaviour is
/// preserved. This is what makes the guard opt-in for unit tests /
/// headless CI / binaries with no hotkey subsystem.
#[test]
fn inject_without_guard_delegates_unchanged() {
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let backend = backend_with(InjectMethod::Typing, fake);
    // Deliberately NO with_injection_guard call.
    backend.inject("plain").expect("inject ok");
    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded.iter().any(|e| e == "type:plain"),
        "type event must reach backend even with no guard installed, got {recorded:?}"
    );
}
