//! Tests for [`super::EnigoInjectBackend`].
//!
//! Mock the dispatcher at the [`InjectorBackend`](crate::injection::enigo_backend::InjectorBackend)
//! boundary so we never actually synthesise keystrokes — the
//! `with_backend` hook on [`Injector`] is the production-supported
//! injection point for exactly this purpose. The tests cover:
//!
//! - delegation: typing / pasting flows reach the underlying backend
//!   with the correct method.
//! - error mapping: a backend failure surfaces as
//!   `InjectError::Backend(_)` with the message preserved.
//! - method accessor: the configured [`InjectMethod`] round-trips.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};

use super::EnigoInjectBackend;
use crate::dictate::session::types::{InjectBackend, InjectError};
use crate::injection::enigo_backend::InjectorBackend;
use crate::injection::{InjectMethod, Injector, PasteShortcut};

/// Recording fake — captures every call made by [`Injector`] so the
/// assertion can pin down what the trait impl actually delegates. Lives
/// behind `Arc<Mutex<…>>` so the test retains a handle after the value
/// is moved into the [`Injector`].
#[derive(Default, Clone)]
struct RecordingBackend {
    events: Arc<Mutex<Vec<String>>>,
    fail_next: Arc<Mutex<Option<String>>>,
}

impl RecordingBackend {
    fn new() -> Self {
        Self::default()
    }

    fn failing(msg: &str) -> Self {
        let me = Self::default();
        *me.fail_next.lock().unwrap() = Some(msg.to_owned());
        me
    }

    fn take_fail(&self) -> Option<String> {
        self.fail_next.lock().unwrap().take()
    }
}

impl InjectorBackend for RecordingBackend {
    fn type_text(&mut self, text: &str) -> Result<()> {
        if let Some(msg) = self.take_fail() {
            return Err(anyhow!("recorded failure: {msg}"));
        }
        self.events.lock().unwrap().push(format!("type:{text}"));
        Ok(())
    }
    fn key_chord(&mut self, modifiers: &[u16], key: u16) -> Result<()> {
        if let Some(msg) = self.take_fail() {
            return Err(anyhow!("recorded failure: {msg}"));
        }
        let mods: Vec<String> = modifiers.iter().map(|m| format!("{m:#x}")).collect();
        self.events
            .lock()
            .unwrap()
            .push(format!("chord:[{}]+{:#x}", mods.join(","), key));
        Ok(())
    }
}

fn backend_with(method: InjectMethod, fake: RecordingBackend) -> EnigoInjectBackend {
    let injector = Injector::new().with_backend(Box::new(fake));
    EnigoInjectBackend::new(injector, method)
}

#[test]
fn inject_typing_delegates_text_to_underlying_backend() {
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let backend = backend_with(InjectMethod::Typing, fake);
    backend.inject("hello world").expect("inject ok");
    assert_eq!(*events.lock().unwrap(), vec!["type:hello world".to_owned()]);
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
    let backend = backend_with(InjectMethod::Paste(Some(PasteShortcut::CtrlV)), fake);
    backend.inject("ignored on paste path").expect("inject ok");
    let recorded = events.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        1,
        "expected exactly one chord event, got {recorded:?}"
    );
    assert!(
        recorded[0].starts_with("chord:["),
        "expected chord event, got {:?}",
        recorded[0]
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
    let backend = backend_with(InjectMethod::Paste(None), fake);
    backend.inject("anything").expect("inject ok");
    let recorded = events.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        1,
        "expected exactly one chord event for Paste(None), got {recorded:?}"
    );
    assert!(
        recorded[0].starts_with("chord:["),
        "expected chord event, got {:?}",
        recorded[0]
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
    assert_eq!(
        *events.lock().unwrap(),
        vec!["type:first".to_owned(), "type:second".to_owned()]
    );
}
