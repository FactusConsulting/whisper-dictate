//! Tests for [`super::ProductionInjectBackend`].
//!
//! Two behaviours over the bare `EnigoInjectBackend`:
//!
//! - **Inject-mode env parse** ([`super::InjectModeChoice::from_env_value`]).
//! - **Print branch:** the print arm of
//!   [`super::ProductionInjectBackend`] skips OS injection. We can't
//!   observe stdout from inside
//!   the test (the default test harness captures it), but we CAN
//!   verify it returns `Ok` and never constructs a recording fake
//!   backend.
//!
//! Modifier release is NOT tested here -- it now lives in
//! `dictate/backends/inject.rs::EnigoInjectBackend` and is covered by
//! the existing tests in `inject_cleanup_tests.rs`. The wrapper just
//! delegates, so verifying the same contract twice would be churn.

use std::sync::{atomic::AtomicBool, Arc, Mutex};
use std::time::Duration;

use anyhow::Result;

use super::{injector_for_xkb_layout, InjectModeChoice, ProductionInjectBackend, INJECT_MODE_ENV};
use crate::dictate::backends::EnigoInjectBackend;
use crate::dictate::session::types::InjectBackend;
use crate::injection::enigo_backend::InjectorBackend;
use crate::injection::paste::Clipboard;
use crate::injection::{InjectMethod, Injector, PasteShortcut};
use crate::platform::foreground_window::WindowInfo;
use crate::test_env_lock::ENV_LOCK;

/// Minimal recording backend for the profile-paste regression test
/// (#619). Captures the sequence of `type_text` / `key_chord`
/// calls so we can assert the paste chord actually fired instead of
/// per-character typing. Duplicates the (private) helper in
/// `dictate::backends::inject_test_support` on purpose so this test
/// stays scope-local -- exposing the shared helper cross-module would
/// pull test-only scaffolding into the public crate API.
#[derive(Default, Clone)]
struct PasteRecordingBackend {
    events: Arc<Mutex<Vec<String>>>,
}

impl InjectorBackend for PasteRecordingBackend {
    fn type_text(&mut self, text: &str) -> Result<()> {
        self.events.lock().unwrap().push(format!("type:{text}"));
        Ok(())
    }
    fn key_chord(&mut self, modifiers: &[u16], key: u16) -> Result<()> {
        let mods: Vec<String> = modifiers.iter().map(|m| format!("{m:#x}")).collect();
        self.events
            .lock()
            .unwrap()
            .push(format!("chord:[{}]+{:#x}", mods.join(","), key));
        Ok(())
    }
    fn release_modifiers(&mut self, modifiers: &[u16]) -> Result<()> {
        let mods: Vec<String> = modifiers.iter().map(|m| format!("{m:#x}")).collect();
        self.events
            .lock()
            .unwrap()
            .push(format!("release:[{}]", mods.join(",")));
        Ok(())
    }
}

#[derive(Default, Clone)]
struct PasteRecordingClipboard {
    writes: Arc<Mutex<Vec<String>>>,
}

impl PasteRecordingClipboard {
    fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }
}

impl Clipboard for PasteRecordingClipboard {
    fn read(&mut self) -> Option<String> {
        None
    }
    fn write(&mut self, value: &str) -> bool {
        self.writes.lock().unwrap().push(value.to_owned());
        true
    }
}

struct FailingClipboard;

impl Clipboard for FailingClipboard {
    fn read(&mut self) -> Option<String> {
        None
    }

    fn write(&mut self, _value: &str) -> bool {
        false
    }
}

#[cfg(not(windows))]
#[derive(Default)]
struct PasteUnavailableBackend {
    typed: Arc<Mutex<Vec<String>>>,
}

#[cfg(not(windows))]
impl InjectorBackend for PasteUnavailableBackend {
    fn type_text(&mut self, text: &str) -> Result<()> {
        self.typed.lock().unwrap().push(text.to_owned());
        Ok(())
    }

    fn key_chord(&mut self, _modifiers: &[u16], _key: u16) -> Result<()> {
        anyhow::bail!("no Linux paste helper found on PATH")
    }
}

// ── env-mode parser ──────────────────────────────────────────────────────────

#[test]
fn env_parser_recognises_print() {
    assert_eq!(
        InjectModeChoice::from_env_value(Some("print")),
        InjectModeChoice::Print
    );
    assert_eq!(
        InjectModeChoice::from_env_value(Some("  PRINT  ")),
        InjectModeChoice::Print,
        "whitespace + case must not block the print branch"
    );
}

#[test]
fn env_parser_recognises_paste() {
    assert_eq!(
        InjectModeChoice::from_env_value(Some("paste")),
        InjectModeChoice::Paste
    );
    assert_eq!(
        InjectModeChoice::from_env_value(Some("Paste")),
        InjectModeChoice::Paste
    );
}

#[test]
fn env_parser_keeps_auto_distinct_from_explicit_typing() {
    assert_eq!(
        InjectModeChoice::from_env_value(Some("type")),
        InjectModeChoice::Typing
    );
    for raw in [None, Some(""), Some("   "), Some("auto"), Some("garbage")] {
        assert_eq!(
            InjectModeChoice::from_env_value(raw),
            InjectModeChoice::Auto,
            "raw={raw:?} must use automatic selection"
        );
    }
}

#[test]
fn auto_wayland_pastes_danish_text_but_types_ascii() {
    assert_eq!(
        super::auto_method_for(
            "Jeg tænkte på døren, så æøå bevares",
            crate::injection::LinuxSession::OtherWayland,
        ),
        InjectMethod::Paste(None),
    );
    assert_eq!(
        super::auto_method_for("plain ASCII", crate::injection::LinuxSession::OtherWayland),
        InjectMethod::Typing,
    );
}

#[test]
fn windows_auto_uses_reliable_paste_for_ascii_and_unicode() {
    for text in ["plain ASCII", "æøå"] {
        assert_eq!(
            super::auto_method_for_platform(
                text,
                "windows",
                crate::injection::LinuxSession::Unknown,
            ),
            InjectMethod::Paste(None),
        );
    }
}

#[test]
#[cfg(windows)]
fn windows_auto_never_falls_back_to_unreliable_typing() {
    let fake = PasteRecordingBackend::default();
    let events = fake.events.clone();
    let enigo = EnigoInjectBackend::new(
        Injector::new().with_backend(Box::new(fake)),
        InjectMethod::Typing,
    )
    .with_clipboard(Box::new(FailingClipboard));

    super::inject_auto(&enigo, "æøå", InjectMethod::Paste(None))
        .expect_err("Windows auto must fail loudly when reliable paste fails");

    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.starts_with("type:")),
        "Windows auto must never fall back to per-character typing"
    );
}

#[test]
#[cfg(not(windows))]
fn auto_retries_typing_when_clipboard_write_is_unavailable() {
    let fake = PasteRecordingBackend::default();
    let events = fake.events.clone();
    let enigo = EnigoInjectBackend::new(
        Injector::new().with_backend(Box::new(fake)),
        InjectMethod::Typing,
    )
    .with_clipboard(Box::new(FailingClipboard));

    super::inject_auto(&enigo, "æøå", InjectMethod::Paste(None))
        .expect("auto must fall back to typing");

    let events = events.lock().unwrap();
    assert!(
        events.iter().any(|event| event == "type:æøå"),
        "fallback must type the full utterance: {events:?}"
    );
    assert!(
        !events.iter().any(|event| event.starts_with("chord:")),
        "clipboard failure happens before any paste chord: {events:?}"
    );
}

#[test]
#[cfg(not(windows))]
fn auto_retries_typing_when_only_a_typing_helper_is_available() {
    let fake = PasteUnavailableBackend::default();
    let typed = fake.typed.clone();
    let enigo = EnigoInjectBackend::new(
        Injector::new().with_backend(Box::new(fake)),
        InjectMethod::Typing,
    )
    .with_clipboard(Box::new(PasteRecordingClipboard::default()));

    super::inject_auto(&enigo, "æøå", InjectMethod::Paste(None))
        .expect("auto must fall back after a no-paste-helper error");

    assert_eq!(*typed.lock().unwrap(), ["æøå"]);
}

// ── print branch ──────────────────────────────────────────────────────────────

#[test]
fn print_variant_does_not_invoke_any_backend() {
    // The Print variant must not even construct an Injector that could
    // accidentally talk to enigo. Build it via for_choice + assert the
    // public InjectBackend impl returns Ok without going through any
    // OS path. We can't observe stdout from inside the test (the
    // default harness captures it) so we assert the success result +
    // pair it with `method()` returning None to prove the path skipped
    // backend construction entirely.
    let backend = ProductionInjectBackend::for_choice(InjectModeChoice::Print);
    backend
        .inject("hello world")
        .expect("print branch must succeed without touching OS");
    assert_eq!(backend.method(), None, "Print has no InjectMethod");
}

// ── from_env wiring ──────────────────────────────────────────────────────────

#[test]
fn from_env_print_value_selects_print_branch() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var(INJECT_MODE_ENV).ok();
    std::env::set_var(INJECT_MODE_ENV, "print");
    let backend = ProductionInjectBackend::from_env().expect("print needs no clipboard");
    assert_eq!(backend.method(), None);
    backend.inject("dry run").expect("print path ok");
    match prev {
        Some(v) => std::env::set_var(INJECT_MODE_ENV, v),
        None => std::env::remove_var(INJECT_MODE_ENV),
    }
}

#[test]
fn paste_and_windows_auto_initialize_a_clipboard() {
    for (raw, expected_mode) in [
        (Some("paste"), InjectModeChoice::Paste),
        (Some("auto"), InjectModeChoice::Auto),
        (None, InjectModeChoice::Auto),
    ] {
        let called = std::cell::Cell::new(false);
        let backend = ProductionInjectBackend::for_env_value_with_clipboard(raw, "windows", || {
            called.set(true);
            Ok(Box::new(PasteRecordingClipboard::default()))
        })
        .expect("recording clipboard initializes");
        assert!(called.get(), "raw={raw:?} must initialize the clipboard");
        assert_eq!(backend.active_mode(), expected_mode);
    }
}

#[test]
fn every_profile_switchable_mode_requires_a_clipboard_at_startup() {
    for (raw, os) in [
        (Some("auto"), "windows"),
        (Some("type"), "linux"),
        (Some("print"), "macos"),
    ] {
        let err = ProductionInjectBackend::for_env_value_with_clipboard(raw, os, || {
            Err("clipboard unavailable".to_owned())
        })
        .expect_err("a later profile may select paste from every starting mode");
        assert!(err.contains("clipboard unavailable"), "raw={raw:?} os={os}");
    }
}
// ── explicit-paste helper ────────────────────────────────────────────────────

// ── profile-override plumbing (#607) ──────────────────────────────

#[test]
fn profile_inject_mode_override_flips_active_mode_for_next_utterance() {
    // #607: a profile that carries `inject_mode=print` must
    // flip the active mode to Print for the NEXT inject call, even when
    // the backend was constructed with Typing. Uses `active_mode()`
    // (test-only) to snapshot the Mutex without a full session cycle.
    let backend = ProductionInjectBackend::for_choice(InjectModeChoice::Typing);
    assert_eq!(backend.active_mode(), InjectModeChoice::Typing);
    let mut profile = std::collections::BTreeMap::new();
    profile.insert("inject_mode".to_owned(), "print".to_owned());
    backend.apply_profile_overrides(&profile);
    assert_eq!(
        backend.active_mode(),
        InjectModeChoice::Print,
        "profile inject_mode=print must swap the Mutex slot"
    );
    // Reset semantics: an empty profile map must snap back to the
    // ambient env-driven choice so a fired profile does not leak into
    // the next utterance.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var(INJECT_MODE_ENV).ok();
    std::env::remove_var(INJECT_MODE_ENV);
    backend.apply_profile_overrides(&std::collections::BTreeMap::new());
    assert_eq!(
        backend.active_mode(),
        InjectModeChoice::Auto,
        "empty profile map must reset to the ambient (unset -> Auto) env mode"
    );
    if let Some(v) = prev {
        std::env::set_var(INJECT_MODE_ENV, v);
    }
}

#[test]
fn profile_inject_mode_upgrade_from_print_to_type_is_supported() {
    // The struct always constructs an EnigoInjectBackend so a profile
    // can upgrade a Print session to Typing (or Paste) at any time --
    // matching Python's live-reload of the `inject_mode` config key.
    let backend = ProductionInjectBackend::for_choice(InjectModeChoice::Print);
    assert_eq!(backend.active_mode(), InjectModeChoice::Print);
    let mut profile = std::collections::BTreeMap::new();
    profile.insert("inject_mode".to_owned(), "type".to_owned());
    backend.apply_profile_overrides(&profile);
    assert_eq!(backend.active_mode(), InjectModeChoice::Typing);
}

#[test]
fn explicit_paste_shortcut_round_trips() {
    let backend = ProductionInjectBackend::with_explicit_paste_shortcut(PasteShortcut::CtrlShiftV);
    assert_eq!(
        backend.method(),
        Some(InjectMethod::Paste(Some(PasteShortcut::CtrlShiftV)))
    );
}

#[test]
fn profile_inject_mode_override_from_typing_to_paste_actually_pastes() {
    // #619 runtime/rust_session_inject.rs:146. The regression:
    // a saved user profile with `inject_mode=paste` used to flip the
    // wrapper's Mutex slot to `Paste` while the underlying
    // `EnigoInjectBackend` stayed on the constructor's `Typing`
    // method, so `inject()` silently typed the transcript
    // character-by-character. This test drives the fix end-to-end:
    // build a `ProductionInjectBackend` starting on `Typing`, apply a
    // profile with `inject_mode=paste`, call `inject()`, and assert
    // the recording backend saw a paste CHORD (not per-character typing)
    // and that the clipboard was populated.
    let fake = PasteRecordingBackend::default();
    let events = fake.events.clone();
    let clipboard = PasteRecordingClipboard::default();
    let clipboard_probe = clipboard.clone();
    let injector = Injector::new().with_backend(Box::new(fake));
    let enigo = EnigoInjectBackend::new(injector, InjectMethod::Typing)
        .with_clipboard(Box::new(clipboard))
        .with_restore_delay(Duration::ZERO);
    let backend = ProductionInjectBackend::with_enigo_for_test(InjectModeChoice::Typing, enigo);

    // Sanity: before the override, the wrapper reports Typing and
    // routes through the typing branch. We do not inject here -- the
    // regression is specifically about what happens AFTER the profile
    // flips the mode, so keep the pre-state to `method()` only.
    assert_eq!(backend.method(), Some(InjectMethod::Typing));

    // Apply a profile that carries `inject_mode=paste` -- the exact
    // key/value contract `SessionConfig::from_profile_overrides` reads
    // from the matched profile.
    let mut profile = std::collections::BTreeMap::new();
    profile.insert("inject_mode".to_owned(), "paste".to_owned());
    backend.apply_profile_overrides(&profile);

    // Effective method now reports Paste -- the fix propagates the
    // override down to the enigo dispatch.
    assert_eq!(
        backend.method(),
        Some(InjectMethod::Paste(None)),
        "profile inject_mode=paste must flip the effective method to Paste, \
         not silently keep Typing"
    );

    // Actually inject and observe: the recorder must see a paste chord
    // (release-modifiers sweep + `chord:[...]+..`), NOT a `type:` event.
    backend
        .inject("profile-paste-text")
        .expect("paste with clipboard wired must succeed");

    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded.iter().any(|e| e.starts_with("chord:[")),
        "profile paste override MUST emit a paste chord, got events: {recorded:?}"
    );
    assert!(
        !recorded.iter().any(|e| e.starts_with("type:")),
        "profile paste override MUST NOT fall through to per-character typing; \
         got events: {recorded:?}"
    );

    // The transcript reached the clipboard via the paste guard.
    assert_eq!(
        clipboard_probe.writes(),
        vec!["profile-paste-text".to_owned()],
        "paste path must copy the transcript to the clipboard before the chord fires"
    );
}

#[test]
fn native_injector_carries_the_effective_xkb_layout() {
    let configured = format!("{:?}", injector_for_xkb_layout(Some(" dk ")));
    assert!(
        configured.contains("xkb_layout: \"dk\""),
        "configured layout must reach the native injection dispatcher: {configured}"
    );

    let automatic = format!("{:?}", injector_for_xkb_layout(None));
    assert!(
        automatic.contains("xkb_layout: \"\""),
        "unset layout must preserve auto-detection: {automatic}"
    );
}

#[test]
fn stopped_runtime_blocks_in_flight_text_before_any_injection_side_effect() {
    let fake = PasteRecordingBackend::default();
    let events = fake.events.clone();
    let clipboard = PasteRecordingClipboard::default();
    let clipboard_probe = clipboard.clone();
    let injector = Injector::new().with_backend(Box::new(fake));
    let enigo = EnigoInjectBackend::new(injector, InjectMethod::Paste(None))
        .with_clipboard(Box::new(clipboard))
        .with_restore_delay(Duration::ZERO);
    let active = Arc::new(AtomicBool::new(false));
    let backend = ProductionInjectBackend::with_enigo_and_activity_for_test(
        InjectModeChoice::Paste,
        enigo,
        active,
    );

    backend
        .inject("must-not-escape-after-stop")
        .expect("lifecycle suppression is a successful no-op");

    assert!(
        events.lock().unwrap().is_empty(),
        "a stopped runtime must not reach keyboard injection"
    );
    assert!(
        clipboard_probe.writes().is_empty(),
        "a stopped runtime must not write the transcript to the clipboard"
    );
}

#[test]
fn stopped_runtime_skips_captured_target_restoration() {
    let backend = ProductionInjectBackend::with_enigo_and_activity_for_test(
        InjectModeChoice::Typing,
        EnigoInjectBackend::new(Injector::new(), InjectMethod::Typing),
        Arc::new(AtomicBool::new(false)),
    );
    let target = WindowInfo::new(Some("Editor".to_owned()), Some("editor.exe".to_owned()))
        .with_target_id(Some("captured-target".to_owned()));

    backend
        .prepare_target(Some(&target))
        .expect("stopped target restoration is a no-op");
}
