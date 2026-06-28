//! Tests for [`super::ProductionInjectBackend`].
//!
//! Two behaviours over the bare `EnigoInjectBackend`:
//!
//! - **Inject-mode env parse** ([`super::InjectModeChoice::from_env_value`]).
//! - **Print branch:** the [`super::ProductionInjectBackend::Print`]
//!   variant skips OS injection. We can't observe stdout from inside
//!   the test (the default test harness captures it), but we CAN
//!   verify it returns `Ok` and never constructs a recording fake
//!   backend.
//!
//! Modifier release is NOT tested here -- it now lives in
//! `dictate/backends/inject.rs::EnigoInjectBackend` and is covered by
//! the existing tests in `inject_cleanup_tests.rs`. The wrapper just
//! delegates, so verifying the same contract twice would be churn.

use super::{InjectModeChoice, ProductionInjectBackend, INJECT_MODE_ENV};
use crate::dictate::session::types::InjectBackend;
use crate::injection::{InjectMethod, PasteShortcut};
use crate::test_env_lock::ENV_LOCK;

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
fn env_parser_collapses_type_auto_empty_unknown_to_typing() {
    // The PR-5 sink picks Typing for everything that is not explicitly
    // print / paste so the auto + unknown paths do not silently switch
    // to a behaviour the user did not pick. Matches the Python fallback
    // in `vp_cli.py:VALID_INJECT_MODES`.
    for raw in [
        None,
        Some(""),
        Some("   "),
        Some("type"),
        Some("auto"),
        Some("garbage"),
    ] {
        assert_eq!(
            InjectModeChoice::from_env_value(raw),
            InjectModeChoice::Typing,
            "raw={raw:?} must collapse to Typing"
        );
    }
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
    let backend = ProductionInjectBackend::from_env();
    assert_eq!(
        backend.method(),
        None,
        "VOICEPI_INJECT_MODE=print must pick Print"
    );
    backend.inject("dry run").expect("print path ok");
    match prev {
        Some(v) => std::env::set_var(INJECT_MODE_ENV, v),
        None => std::env::remove_var(INJECT_MODE_ENV),
    }
}

#[test]
fn from_env_paste_value_currently_collapses_to_typing() {
    // Paste mode is documented to collapse to Typing in this PR -- the
    // underlying `EnigoInjectBackend` requires a `Clipboard` backend
    // wired via `with_clipboard` to drive the paste arm (Codex P1 #419
    // inject.rs:266) and the rust-session sink does not own a
    // Clipboard impl yet. Pin the contract so the follow-up that
    // wires `arboard` knows which test to flip.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var(INJECT_MODE_ENV).ok();
    std::env::set_var(INJECT_MODE_ENV, "paste");
    let backend = ProductionInjectBackend::from_env();
    assert_eq!(
        backend.method(),
        Some(InjectMethod::Typing),
        "paste collapses to Typing in this PR; follow-up will wire a Clipboard"
    );
    match prev {
        Some(v) => std::env::set_var(INJECT_MODE_ENV, v),
        None => std::env::remove_var(INJECT_MODE_ENV),
    }
}

#[test]
fn from_env_missing_value_defaults_to_typing() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var(INJECT_MODE_ENV).ok();
    std::env::remove_var(INJECT_MODE_ENV);
    let backend = ProductionInjectBackend::from_env();
    assert_eq!(
        backend.method(),
        Some(InjectMethod::Typing),
        "unset VOICEPI_INJECT_MODE must fall back to Typing"
    );
    if let Some(v) = prev {
        std::env::set_var(INJECT_MODE_ENV, v);
    }
}

// ── explicit-paste helper ────────────────────────────────────────────────────

#[test]
fn explicit_paste_shortcut_round_trips() {
    let backend = ProductionInjectBackend::with_explicit_paste_shortcut(PasteShortcut::CtrlShiftV);
    assert_eq!(
        backend.method(),
        Some(InjectMethod::Paste(Some(PasteShortcut::CtrlShiftV)))
    );
}
