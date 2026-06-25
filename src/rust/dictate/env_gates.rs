//! Env-flag parsing helpers: mirror of `runtime._truthy`,
//! `runtime._config_dump_enabled`, `runtime._trace_enabled` in
//! `src/python/whisper_dictate/runtime.py`.
//!
//! Pure: callers pass the already-fetched env values (or `None`),
//! so the functions stay testable without mutating the process env.

/// `True` when the value is non-empty and not one of the "off-ish"
/// strings Python accepts in config flags. Mirrors `runtime._truthy`
/// byte-for-byte: strip, lowercase, reject the disable set
/// `("", "0", "false", "no", "off")`.
pub fn is_truthy(value: Option<&str>) -> bool {
    let trimmed = value.unwrap_or("").trim().to_lowercase();
    !matches!(trimmed.as_str(), "" | "0" | "false" | "no" | "off")
}

/// True when the startup `[debug] effective settings:` config dump should
/// be printed. Mirrors `runtime._config_dump_enabled`: BOTH `VOICEPI_DEBUG`
/// and `VOICEPI_STT_DEBUG` must be truthy (Verbose level).
pub fn config_dump_enabled(voicepi_debug: Option<&str>, voicepi_stt_debug: Option<&str>) -> bool {
    is_truthy(voicepi_debug) && is_truthy(voicepi_stt_debug)
}

/// True when the Trace-level diagnostics should run (startup
/// audio-device dump + per-attempt capture logging). Mirrors
/// `runtime._trace_enabled`: just `VOICEPI_TRACE` being truthy.
pub fn trace_enabled(voicepi_trace: Option<&str>) -> bool {
    is_truthy(voicepi_trace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_recognises_non_empty_value() {
        assert!(is_truthy(Some("1")));
        assert!(is_truthy(Some("on")));
        assert!(is_truthy(Some("yes")));
        assert!(is_truthy(Some("true")));
        // Anything not in the disable set counts as truthy.
        assert!(is_truthy(Some("anything-else")));
    }

    #[test]
    fn falsy_inputs_are_rejected() {
        for v in ["", "0", "false", "FALSE", " False ", "no", "off"] {
            assert!(!is_truthy(Some(v)), "expected {v:?} to be falsy");
        }
        assert!(!is_truthy(None));
    }

    #[test]
    fn trim_and_case_insensitive() {
        assert!(!is_truthy(Some("  Off  ")));
        assert!(is_truthy(Some("  YES  ")));
    }

    #[test]
    fn config_dump_requires_both_flags() {
        assert!(!config_dump_enabled(None, None));
        assert!(!config_dump_enabled(Some("1"), None));
        assert!(!config_dump_enabled(None, Some("1")));
        assert!(!config_dump_enabled(Some("1"), Some("off")));
        assert!(config_dump_enabled(Some("1"), Some("1")));
    }

    #[test]
    fn trace_follows_truthy() {
        assert!(!trace_enabled(None));
        assert!(!trace_enabled(Some("0")));
        assert!(trace_enabled(Some("1")));
        assert!(trace_enabled(Some("on")));
    }
}
