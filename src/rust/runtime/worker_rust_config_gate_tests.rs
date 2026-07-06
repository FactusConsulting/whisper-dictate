//! Sibling test file for
//! [`super::worker_rust::unsupported_worker_rust_settings_reason`]
//! added in PR #441 review round 2 (Codex P1 findings 1 + 5). Split
//! out of `worker_rust_tests.rs` so both files stay under the AGENTS.md
//! ~500-LOC-per-file modularity guideline. Pins the config gate that
//! keeps unsupported STT / post-processing configurations on the
//! Python orchestrator; each test targets a single field so a
//! Wave-5.5 patch that ports one path (postprocess, format commands,
//! cloud STT) can drop the matching arm in `worker_rust.rs` and see
//! exactly which assertion needs to be rewritten.
//!
//! `dictionary_enabled` is a KNOWN Wave-5.5 gap but is deliberately
//! NOT gated -- see [`unsupported_reason_none_on_defaults`] and
//! [`unsupported_reason_ignores_dictionary_enabled`] for the pinned
//! contract.

use super::worker_rust::unsupported_worker_rust_settings_reason;
use crate::config::AppSettings;

#[test]
fn unsupported_reason_none_on_defaults() {
    // Load-bearing baseline: `AppSettings::default()` must produce
    // None so PR 7's default flip ("Rust worker on fresh installs")
    // actually takes effect. If a future patch added a config gate on
    // a field that defaults to `true` (like `dictionary_enabled`),
    // every fresh install would silently fall back to Python and this
    // test would fire loudly. See the `dictionary_enabled` NOTE in
    // `unsupported_worker_rust_settings_reason` -- that field is a
    // known Wave-5.5 gap but is deliberately NOT gated for this
    // reason.
    let settings = AppSettings::default();
    assert!(
        unsupported_worker_rust_settings_reason(&settings).is_none(),
        "AppSettings::default() must be a supported delegate config;          got: {:?}",
        unsupported_worker_rust_settings_reason(&settings)
    );
}

#[test]
fn unsupported_reason_flags_cloud_stt_backend() {
    // Codex P1 finding 1: cloud STT (openai, groq, ...) is not wired
    // through the Rust session yet; delegating would silently switch
    // the user to local Whisper or produce empty dictations.
    let mut settings = AppSettings::default();
    settings.stt_backend = "openai".to_owned();
    let reason = unsupported_worker_rust_settings_reason(&settings)
        .expect("cloud STT must gate off delegation");
    assert!(
        reason.contains("stt_backend"),
        "reason must name the offending field so the stderr log is          actionable; got: {reason}"
    );
    // Case-insensitive match: a saved "OpenAI" (from an editor that
    // did not lowercase) must still trip the gate.
    settings.stt_backend = "OpenAI".to_owned();
    assert!(unsupported_worker_rust_settings_reason(&settings).is_some());
}

#[test]
fn unsupported_reason_flags_configured_post_processor() {
    // Codex P1 postprocess finding: DictateSession does NOT invoke
    // postprocess::run::run yet. Any configured processor gates off
    // delegation.
    for processor in ["ollama", "openai", "groq"] {
        let mut settings = AppSettings::default();
        settings.post_processor = processor.to_owned();
        let reason = unsupported_worker_rust_settings_reason(&settings)
            .unwrap_or_else(|| panic!("post_processor={processor} must gate"));
        assert!(
            reason.contains("post_processor"),
            "reason must name post_processor for {processor}; got: {reason}"
        );
    }
    // "none" is the default and must remain the ONLY supported value.
    let mut settings = AppSettings::default();
    settings.post_processor = "NONE".to_owned();
    assert!(
        unsupported_worker_rust_settings_reason(&settings).is_none(),
        "case-insensitive 'NONE' must match the default (no gate)"
    );
}

#[test]
fn unsupported_reason_flags_configured_format_commands() {
    // Codex P1 postprocess finding: apply_format_commands is not
    // wired from the session either. Any non-"off" value gates.
    for lang in ["en", "da", "both"] {
        let mut settings = AppSettings::default();
        settings.format_commands = lang.to_owned();
        let reason = unsupported_worker_rust_settings_reason(&settings)
            .unwrap_or_else(|| panic!("format_commands={lang} must gate"));
        assert!(
            reason.contains("format_commands"),
            "reason must name format_commands for {lang}; got: {reason}"
        );
    }
    // "off" is the default and must remain the ONLY supported value.
    let mut settings = AppSettings::default();
    settings.format_commands = "OFF".to_owned();
    assert!(unsupported_worker_rust_settings_reason(&settings).is_none());
}

#[test]
fn unsupported_reason_ignores_dictionary_enabled() {
    // `dictionary_enabled` is a known Wave-5.5 gap but is
    // deliberately NOT gated so PR 7's default flip works on fresh
    // installs (see the NOTE in `unsupported_worker_rust_settings_reason`
    // + the paired `unsupported_reason_none_on_defaults` test). Pin
    // the non-gate contract explicitly so a future patch that
    // reintroduces the gate has to update this test AND the doc
    // comment together.
    let mut settings = AppSettings::default();
    settings.dictionary_enabled = true;
    assert!(
        unsupported_worker_rust_settings_reason(&settings).is_none(),
        "dictionary_enabled=true must NOT gate delegation for now (soft          degradation, tracked as Wave-5.5); got: {:?}",
        unsupported_worker_rust_settings_reason(&settings)
    );
    settings.dictionary_enabled = false;
    assert!(unsupported_worker_rust_settings_reason(&settings).is_none());
}

#[test]
fn unsupported_reason_first_offender_wins() {
    // When multiple fields are unsupported, the helper returns the
    // FIRST reason it encounters (stt_backend > post_processor >
    // format_commands). This ordering lets the stderr log point users
    // at the most consequential setting first (a wrong stt_backend is
    // a bigger surprise than a lost format command).
    let mut settings = AppSettings::default();
    settings.stt_backend = "openai".to_owned();
    settings.post_processor = "ollama".to_owned();
    settings.format_commands = "both".to_owned();
    let reason =
        unsupported_worker_rust_settings_reason(&settings).expect("multi-flag config must gate");
    assert!(
        reason.starts_with("stt_backend"),
        "stt_backend must be reported first when multiple flags are set;          got: {reason}"
    );
}
