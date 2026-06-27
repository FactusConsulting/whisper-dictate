//! STT backend validation + human label resolution.
//!
//! Mirrors `runtime._resolve_backend_and_device` + `runtime._resolve_model_name`
//! in `src/python/whisper_dictate/runtime.py`. These run once per startup
//! (right before the model load), so a shell-out cost is negligible — the
//! `dictate-ops` JSON-RPC subcommand exposes both as ops the Python caller
//! can opt into via `VOICEPI_DICTATE_BACKEND=rust`.
//!
//! Wave 8 of #348 removed the NeMo/Parakeet backend; only Whisper (local
//! faster-whisper or the Rust whisper-rs helper) and OpenAI-compatible
//! cloud STT remain.

use std::fmt;

/// The two backends recognised by the worker. Matches
/// `vp_transcribe.VALID_STT_BACKENDS = ("whisper", "openai")` after the
/// Wave 8 of #348 backend removal — the historical alias `"faster-whisper"`
/// is normalised to `"whisper"` at the env-read site
/// (`vp_transcribe.STT_BACKEND`) so it is not part of the public set here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    Whisper,
    Openai,
}

impl BackendKind {
    /// Canonical lowercase identifier, matches the env-var string and the
    /// JSON envelope wire format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Whisper => "whisper",
            Self::Openai => "openai",
        }
    }

    /// Human-facing label printed by `loading {label} {model} …` at
    /// startup. Mirrors `runtime._resolve_model_name`.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Whisper => "Whisper",
            Self::Openai => "External API",
        }
    }

    /// Slice of every valid backend identifier (stable order — used by
    /// the validation error message).
    pub fn all() -> &'static [BackendKind] {
        &[BackendKind::Whisper, BackendKind::Openai]
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Validation failure surfaced as the user-visible
/// `invalid VOICEPI_STT_BACKEND=...; expected one of ...` message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid VOICEPI_STT_BACKEND={input:?}; expected one of {}",
    expected_csv()
)]
pub struct BackendLabelError {
    pub input: String,
}

fn expected_csv() -> String {
    BackendKind::all()
        .iter()
        .map(|b| b.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse + validate a backend identifier (case-insensitive; the historical
/// `"faster-whisper"` alias is mapped to [`BackendKind::Whisper`], matching
/// `vp_transcribe.STT_BACKEND` normalisation). The legacy
/// `"parakeet"` backend was removed in Wave 8 of #348 and now errors out
/// the same as any unknown value.
pub fn validate_backend(input: &str) -> Result<BackendKind, BackendLabelError> {
    match input.trim().to_lowercase().as_str() {
        "whisper" | "faster-whisper" => Ok(BackendKind::Whisper),
        "openai" => Ok(BackendKind::Openai),
        _ => Err(BackendLabelError {
            input: input.to_owned(),
        }),
    }
}

/// Human label for a (validated) backend identifier — convenience wrapper
/// when the caller only wants the printed string. Returns the same error
/// shape as [`validate_backend`] on an unknown backend.
pub fn backend_label(input: &str) -> Result<&'static str, BackendLabelError> {
    validate_backend(input).map(|b| b.label())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_label() {
        assert_eq!(backend_label("whisper").unwrap(), "Whisper");
    }

    #[test]
    fn openai_label() {
        assert_eq!(backend_label("openai").unwrap(), "External API");
    }

    #[test]
    fn legacy_faster_whisper_alias_maps_to_whisper() {
        assert_eq!(
            validate_backend("faster-whisper").unwrap(),
            BackendKind::Whisper
        );
        assert_eq!(backend_label("faster-whisper").unwrap(), "Whisper");
    }

    #[test]
    fn case_insensitive_and_trims_whitespace() {
        assert_eq!(validate_backend(" WHISPER ").unwrap(), BackendKind::Whisper);
        assert_eq!(validate_backend("OpenAI").unwrap(), BackendKind::Openai);
    }

    #[test]
    fn parakeet_backend_no_longer_validates() {
        // Wave 8 of #348: Parakeet was dropped. A saved `stt_backend = "parakeet"`
        // is migrated to "whisper" at load time (config::load::migrate_parakeet_backend),
        // so this validator should now error for "parakeet" the same as any unknown.
        let err = validate_backend("parakeet").unwrap_err();
        assert_eq!(err.input, "parakeet");
        let msg = err.to_string();
        assert!(msg.contains("parakeet"));
        assert!(!msg.contains("expected one of whisper, parakeet"));
    }

    #[test]
    fn unknown_backend_returns_error_naming_input_and_valid_options() {
        let err = validate_backend("groq").unwrap_err();
        assert_eq!(err.input, "groq");
        let msg = err.to_string();
        assert!(msg.contains("groq"));
        assert!(msg.contains("whisper"));
        assert!(msg.contains("openai"));
    }

    #[test]
    fn all_backends_round_trip_through_as_str() {
        for b in BackendKind::all() {
            assert_eq!(validate_backend(b.as_str()).unwrap(), *b);
        }
    }

    #[test]
    fn display_uses_canonical_identifier() {
        assert_eq!(format!("{}", BackendKind::Whisper), "whisper");
        assert_eq!(format!("{}", BackendKind::Openai), "openai");
    }
}
