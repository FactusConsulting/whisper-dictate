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

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;

use super::{InjectModeChoice, ProductionInjectBackend, INJECT_MODE_ENV};
use crate::dictate::backends::EnigoInjectBackend;
use crate::dictate::session::types::InjectBackend;
use crate::injection::enigo_backend::InjectorBackend;
use crate::injection::paste::Clipboard;
use crate::injection::{InjectMethod, Injector, PasteShortcut};
use crate::test_env_lock::ENV_LOCK;

/// Minimal recording backend for the profile-paste regression test
/// (Codex P1 #619). Captures the sequence of `type_text` / `key_chord`
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
fn env_parser_preserves_explicit_type_on_every_platform() {
    for os in ["windows", "linux", "macos"] {
        assert_eq!(
            InjectModeChoice::from_env_value_for_os(Some("type"), os),
            InjectModeChoice::Typing
        );
    }
}

#[test]
fn windows_auto_blank_and_unknown_choose_reliable_paste() {
    for raw in [None, Some(""), Some("auto"), Some("garbage")] {
        assert_eq!(
            InjectModeChoice::from_env_value_for_os(raw, "windows"),
            InjectModeChoice::Paste,
            "raw={raw:?} must use atomic paste on Windows"
        );
    }
    assert_eq!(
        InjectModeChoice::from_env_value_for_os(Some("auto"), "linux"),
        InjectModeChoice::Typing
    );
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
fn paste_and_windows_auto_are_built_with_a_clipboard() {
    for raw in [Some("paste"), Some("auto"), None] {
        let backend = ProductionInjectBackend::for_env_value_with_clipboard(raw, "windows", || {
            Ok(Box::new(PasteRecordingClipboard::default()))
        })
        .expect("recording clipboard initializes");
        assert_eq!(backend.method(), Some(InjectMethod::Paste(None)));
    }
}

#[test]
fn required_clipboard_failure_stops_startup_but_explicit_type_can_continue() {
    let failed = || Err("clipboard unavailable".to_owned());
    let err =
        ProductionInjectBackend::for_env_value_with_clipboard(Some("auto"), "windows", failed)
            .expect_err("Windows auto must not silently fall back to unreliable typing");
    assert!(err.contains("clipboard unavailable"));

    let typed =
        ProductionInjectBackend::for_env_value_with_clipboard(Some("type"), "linux", || {
            panic!("Linux explicit type must not initialize a clipboard")
        })
        .unwrap();
    assert_eq!(typed.method(), Some(InjectMethod::Typing));
}
// ── explicit-paste helper ────────────────────────────────────────────────────

// ── profile-override plumbing (Codex P1 #607) ──────────────────────────────

#[test]
fn profile_inject_mode_override_flips_active_mode_for_next_utterance() {
    // Codex P1 #607: a profile that carries `inject_mode=print` must
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
        InjectModeChoice::from_env_value(None),
        "empty profile map must reset to the platform ambient default"
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
    // Codex P1 #619 runtime/rust_session_inject.rs:146. The regression:
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
