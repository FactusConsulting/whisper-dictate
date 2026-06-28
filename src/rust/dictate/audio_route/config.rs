//! Env-driven configuration knobs for [`super::AudioRoute`].
//!
//! Split out of `audio_route/mod.rs` so the main file stays under the
//! AGENTS.md ~500 LOC modularity bar (Codex P2 #415 audio_route.rs:530).
//! The split groups everything the route needs to re-read from the
//! environment on every [`super::AudioRoute::start_recording`] into one
//! cohesive module: the env-var name constants, the parsed-config
//! struct, and the parser that mirrors the Python defaults.

/// Env var that caps a single recording's duration in seconds.
/// Mirrors `vp_capture._max_record_s`:
///
/// * unset / unparseable -> [`DEFAULT_MAX_RECORD_S`] (`120` s),
/// * `"0"` (or any non-positive / non-finite value) -> no cap,
/// * positive finite -> that many seconds.
///
/// Live-reload-friendly: read at [`RouteConfig::from_env`] call time
/// so a config reload between presses takes effect on the next
/// recording without a process restart (matches the
/// `live: true` flag on the `min_record_seconds` / `max_record_s`
/// settings in `src/python/whisper_dictate/settings_schema.json`).
pub const MAX_RECORD_ENV: &str = "VOICEPI_MAX_RECORD_S";

/// Default cap in seconds when `VOICEPI_MAX_RECORD_S` is unset OR
/// unparseable. Matches the literal in
/// `src/python/whisper_dictate/vp_capture.py::_max_record_s`.
pub const DEFAULT_MAX_RECORD_S: f64 = 120.0;

/// Env var that sets the per-recording misfire floor (seconds). Mirrors
/// the `min_record_seconds` setting in
/// `src/python/whisper_dictate/settings_schema.json`, which is
/// `live: true` and is applied by Python's
/// `_apply_runtime_module_config`. Read at [`RouteConfig::from_env`]
/// time so a Settings save between PTT presses takes effect on the
/// next recording -- Codex P2 #415 audio_route.rs:250.
pub const MIN_RECORD_ENV: &str = "VOICEPI_MIN_RECORD_SECONDS";

/// Default min-record floor in seconds. Matches the `"0.5"` default in
/// `settings_schema.json` for the `min_record_seconds` setting.
/// (The skip helper clamps the effective floor up to 0.3 s regardless,
/// see [`crate::dictate::skip::MIN_RECORD_FLOOR_S`].)
pub const DEFAULT_MIN_RECORD_S: f64 = 0.5;

/// Configuration knobs for the audio route. All optional -- a default
/// route ([`RouteConfig::default`]) has no cap and uses the
/// [`DEFAULT_MIN_RECORD_S`] floor. Use [`RouteConfig::from_env`] for
/// the env-driven Python defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteConfig {
    /// Hard ceiling on per-recording duration in seconds. `None` =
    /// no cap. Populated by [`RouteConfig::from_env`] from
    /// [`MAX_RECORD_ENV`]; tests usually construct directly.
    pub max_record_seconds: Option<f64>,
    /// Misfire floor in seconds -- clips below this are dropped with
    /// `reason="too_short"` by the session. Populated by
    /// [`RouteConfig::from_env`] from [`MIN_RECORD_ENV`]; mirrored into
    /// the session via
    /// [`crate::dictate::session::DictateSession::update_min_record_seconds`]
    /// on every [`super::AudioRoute::start_recording`] so a live
    /// Settings change takes effect on the next press.
    pub min_record_seconds: f64,
}

impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            max_record_seconds: None,
            min_record_seconds: DEFAULT_MIN_RECORD_S,
        }
    }
}

impl RouteConfig {
    /// Read both live-reloaded knobs from the environment. Parse rules
    /// match the Python helpers:
    ///
    /// * [`MAX_RECORD_ENV`] -- env unset / unparseable falls back to
    ///   [`DEFAULT_MAX_RECORD_S`] (the 120 s Python default); a parsed
    ///   non-positive / non-finite value disables the cap.
    /// * [`MIN_RECORD_ENV`] -- env unset / unparseable / non-finite
    ///   falls back to [`DEFAULT_MIN_RECORD_S`] (0.5 s). Negative
    ///   values clamp to 0 so a misconfigured negative still surfaces
    ///   the absolute 0.3 s misfire floor in the skip helper rather
    ///   than disabling the floor entirely.
    pub fn from_env() -> Self {
        Self {
            max_record_seconds: parse_max_record_seconds(std::env::var(MAX_RECORD_ENV).ok()),
            min_record_seconds: parse_min_record_seconds(std::env::var(MIN_RECORD_ENV).ok()),
        }
    }
}

/// Parse a `VOICEPI_MAX_RECORD_S` env value into the [`RouteConfig`]
/// `max_record_seconds` field. Pulled out so the parse semantics are
/// unit-testable without going through `std::env`.
fn parse_max_record_seconds(raw: Option<String>) -> Option<f64> {
    // Mirror Python's `(os.environ.get(...) or "120").strip()`: an
    // absent variable AND an unparseable string both fall back to the
    // 120 s default. A successfully parsed non-positive value (e.g.
    // `"0"`) is a deliberate "disable the cap" signal.
    let parsed: f64 = match raw.as_deref().map(str::trim) {
        None => DEFAULT_MAX_RECORD_S,
        Some(s) => s.parse::<f64>().unwrap_or(DEFAULT_MAX_RECORD_S),
    };
    Some(parsed).filter(|v| v.is_finite() && *v > 0.0)
}

/// Parse a `VOICEPI_MIN_RECORD_SECONDS` env value into the
/// [`RouteConfig`] `min_record_seconds` field. Negative parsed values
/// clamp to 0 so the absolute 0.3 s skip-helper floor still applies
/// (matches Python: a negative env value would parse to a negative
/// `float`, then `max(0.3, value)` in `vp_dictate.py:347` clamps it
/// back to 0.3 -- we do the clamp here too, before the value reaches
/// the skip helper, to keep semantics consistent across the route).
fn parse_min_record_seconds(raw: Option<String>) -> f64 {
    let parsed: f64 = match raw.as_deref().map(str::trim) {
        None => DEFAULT_MIN_RECORD_S,
        Some(s) => s.parse::<f64>().unwrap_or(DEFAULT_MIN_RECORD_S),
    };
    if parsed.is_finite() && parsed > 0.0 {
        parsed
    } else if parsed.is_finite() {
        0.0
    } else {
        DEFAULT_MIN_RECORD_S
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_record_parser_matches_python_semantics() {
        assert_eq!(parse_max_record_seconds(None), Some(DEFAULT_MAX_RECORD_S));
        assert_eq!(
            parse_max_record_seconds(Some("not-a-number".into())),
            Some(DEFAULT_MAX_RECORD_S),
        );
        assert_eq!(parse_max_record_seconds(Some("0".into())), None);
        assert_eq!(parse_max_record_seconds(Some("-5".into())), None);
        assert_eq!(parse_max_record_seconds(Some("  42  ".into())), Some(42.0));
    }

    #[test]
    fn min_record_parser_matches_python_semantics() {
        // Codex P2 #415 audio_route.rs:250 (round 7-D): the
        // VOICEPI_MIN_RECORD_SECONDS live-reload semantics. Unset /
        // unparseable / non-finite falls back to the 0.5 s default;
        // negative clamps to 0 (the skip helper raises to 0.3 s).
        assert_eq!(parse_min_record_seconds(None), DEFAULT_MIN_RECORD_S);
        assert_eq!(
            parse_min_record_seconds(Some("not-a-number".into())),
            DEFAULT_MIN_RECORD_S,
        );
        assert_eq!(parse_min_record_seconds(Some("0.8".into())), 0.8);
        assert_eq!(parse_min_record_seconds(Some("  1.25  ".into())), 1.25);
        assert_eq!(
            parse_min_record_seconds(Some("-0.1".into())),
            0.0,
            "negative values clamp to 0 -- the skip helper applies the 0.3 s absolute floor",
        );
        assert_eq!(parse_min_record_seconds(Some("0".into())), 0.0);
    }
}
