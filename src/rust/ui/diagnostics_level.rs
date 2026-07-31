//! Pure mapping between persisted native `log_level` values and the
//! user-facing Diagnostics dropdown.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum DiagnosticsLevel {
    Off,
    Basic,
    Verbose,
    Trace,
}

impl DiagnosticsLevel {
    pub(in crate::ui) const ALL: [DiagnosticsLevel; 4] = [
        DiagnosticsLevel::Off,
        DiagnosticsLevel::Basic,
        DiagnosticsLevel::Verbose,
        DiagnosticsLevel::Trace,
    ];
}

pub(in crate::ui) fn diagnostics_level(raw: &str) -> DiagnosticsLevel {
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" => DiagnosticsLevel::Off,
        "debug" => DiagnosticsLevel::Verbose,
        "trace" => DiagnosticsLevel::Trace,
        _ => DiagnosticsLevel::Basic,
    }
}

pub(in crate::ui) fn apply_diagnostics_level(level: DiagnosticsLevel) -> &'static str {
    match level {
        DiagnosticsLevel::Off => "off",
        DiagnosticsLevel::Basic => "info",
        DiagnosticsLevel::Verbose => "debug",
        DiagnosticsLevel::Trace => "trace",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_log_levels_round_trip_through_dropdown() {
        for (raw, level) in [
            ("off", DiagnosticsLevel::Off),
            ("info", DiagnosticsLevel::Basic),
            ("debug", DiagnosticsLevel::Verbose),
            ("trace", DiagnosticsLevel::Trace),
        ] {
            assert_eq!(diagnostics_level(raw), level);
            assert_eq!(apply_diagnostics_level(level), raw);
        }
    }

    #[test]
    fn unknown_level_falls_back_to_safe_info_tier() {
        assert_eq!(diagnostics_level("unknown"), DiagnosticsLevel::Basic);
        assert_eq!(diagnostics_level(""), DiagnosticsLevel::Basic);
    }
}
