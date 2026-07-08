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
    let settings = AppSettings::default();
    assert!(
        unsupported_worker_rust_settings_reason(&settings).is_none(),
        "AppSettings::default() must be a supported delegate config; got: {:?}",
        unsupported_worker_rust_settings_reason(&settings)
    );
}

#[test]
fn unsupported_reason_flags_cloud_stt_backend() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        ..AppSettings::default()
    };
    let reason = unsupported_worker_rust_settings_reason(&settings)
        .expect("cloud STT must gate off delegation");
    assert!(
        reason.contains("stt_backend"),
        "reason must name the offending field so the stderr log is actionable; got: {reason}"
    );
    let settings = AppSettings {
        stt_backend: "OpenAI".to_owned(),
        ..AppSettings::default()
    };
    assert!(unsupported_worker_rust_settings_reason(&settings).is_some());
}

#[test]
fn unsupported_reason_flags_configured_post_processor() {
    for processor in ["ollama", "openai", "groq"] {
        let settings = AppSettings {
            post_processor: processor.to_owned(),
            ..AppSettings::default()
        };
        let reason = unsupported_worker_rust_settings_reason(&settings)
            .unwrap_or_else(|| panic!("post_processor={processor} must gate"));
        assert!(
            reason.contains("post_processor"),
            "reason must name post_processor for {processor}; got: {reason}"
        );
    }
    let settings = AppSettings {
        post_processor: "NONE".to_owned(),
        ..AppSettings::default()
    };
    assert!(
        unsupported_worker_rust_settings_reason(&settings).is_none(),
        "case-insensitive 'NONE' must match the default (no gate)"
    );
}

#[test]
fn unsupported_reason_flags_configured_format_commands() {
    for lang in ["en", "da", "both"] {
        let settings = AppSettings {
            format_commands: lang.to_owned(),
            ..AppSettings::default()
        };
        let reason = unsupported_worker_rust_settings_reason(&settings)
            .unwrap_or_else(|| panic!("format_commands={lang} must gate"));
        assert!(
            reason.contains("format_commands"),
            "reason must name format_commands for {lang}; got: {reason}"
        );
    }
    let settings = AppSettings {
        format_commands: "OFF".to_owned(),
        ..AppSettings::default()
    };
    assert!(unsupported_worker_rust_settings_reason(&settings).is_none());
}

#[test]
fn unsupported_reason_ignores_dictionary_enabled() {
    let settings = AppSettings {
        dictionary_enabled: true,
        ..AppSettings::default()
    };
    assert!(
        unsupported_worker_rust_settings_reason(&settings).is_none(),
        "dictionary_enabled=true must NOT gate delegation for now; got: {:?}",
        unsupported_worker_rust_settings_reason(&settings)
    );
    let settings = AppSettings {
        dictionary_enabled: false,
        ..AppSettings::default()
    };
    assert!(unsupported_worker_rust_settings_reason(&settings).is_none());
}

#[test]
fn unsupported_reason_first_offender_wins() {
    let settings = AppSettings {
        stt_backend: "openai".to_owned(),
        post_processor: "ollama".to_owned(),
        format_commands: "both".to_owned(),
        ..AppSettings::default()
    };
    let reason =
        unsupported_worker_rust_settings_reason(&settings).expect("multi-flag config must gate");
    assert!(
        reason.starts_with("stt_backend"),
        "stt_backend must be reported first when multiple flags are set; got: {reason}"
    );
}
