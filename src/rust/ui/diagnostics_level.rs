//! Pure mapping between the two persisted debug bools (`debug`, `stt_debug`)
//! and a single user-facing **Diagnostics level** (Off / Basic / Verbose).
//!
//! The Output tab used to expose the two raw env-named toggles directly, which
//! read as confusing low-level switches. They are kept untouched in the config,
//! the env vars and the worker — this module is a *pure UI affordance* that
//! presents them as one ordered "how much output" dropdown.
//!
//! Mapping (see [`diagnostics_level`] / [`apply_diagnostics_level`]):
//!
//! | Level   | `debug` | `stt_debug` | meaning                                  |
//! |---------|---------|-------------|------------------------------------------|
//! | Off     | false   | false       | no diagnostics                           |
//! | Basic   | true    | false       | concise per-utterance `[health]` line    |
//! | Verbose | true    | true        | + startup config dump + per-segment STT  |
//!
//! Basic (`debug:on`) emits one plain-language `[health]` line per utterance
//! (mic level/SNR + model confidence + terse warnings). Verbose adds the startup
//! effective-settings dump and the per-segment STT/dictionary detail on top.
//!
//! The fourth raw combo (`debug:false, stt_debug:true`) is *impossible* via the
//! dropdown but can occur in a hand-edited config. Any `stt_debug` implies at
//! least Verbose output, so that combo is bucketed as **Verbose** — the most
//! informative coherent value — rather than silently dropping the stt detail.

/// Ordered diagnostics verbosity surfaced by the Output-tab dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum DiagnosticsLevel {
    /// No diagnostics: `debug:off, stt_debug:off`.
    Off,
    /// Concise per-utterance `[health]` line: `debug:on, stt_debug:off`.
    Basic,
    /// Health line + startup config dump + per-segment STT/dictionary detail:
    /// `debug:on, stt_debug:on`.
    Verbose,
}

impl DiagnosticsLevel {
    /// Off → Basic → Verbose, ascending, for rendering the dropdown options.
    pub(in crate::ui) const ALL: [DiagnosticsLevel; 3] = [
        DiagnosticsLevel::Off,
        DiagnosticsLevel::Basic,
        DiagnosticsLevel::Verbose,
    ];
}

/// Derive the user-facing level from the two persisted bools.
///
/// `stt_debug` always implies at least Verbose (it adds per-segment detail on
/// top of the startup dump), so the impossible `(false, true)` combo is read as
/// Verbose rather than Off — the dropdown then shows a coherent value.
pub(in crate::ui) fn diagnostics_level(debug: bool, stt_debug: bool) -> DiagnosticsLevel {
    match (debug, stt_debug) {
        (_, true) => DiagnosticsLevel::Verbose,
        (true, false) => DiagnosticsLevel::Basic,
        (false, false) => DiagnosticsLevel::Off,
    }
}

/// Expand a chosen level back into the `(debug, stt_debug)` bools to persist.
pub(in crate::ui) fn apply_diagnostics_level(level: DiagnosticsLevel) -> (bool, bool) {
    match level {
        DiagnosticsLevel::Off => (false, false),
        DiagnosticsLevel::Basic => (true, false),
        DiagnosticsLevel::Verbose => (true, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_from_bools_canonical_combos() {
        assert_eq!(diagnostics_level(false, false), DiagnosticsLevel::Off);
        assert_eq!(diagnostics_level(true, false), DiagnosticsLevel::Basic);
        assert_eq!(diagnostics_level(true, true), DiagnosticsLevel::Verbose);
    }

    #[test]
    fn impossible_combo_is_treated_as_verbose() {
        // debug:off + stt_debug:on can only come from a hand-edited config; any
        // stt_debug implies at least Verbose so the dropdown stays coherent.
        assert_eq!(diagnostics_level(false, true), DiagnosticsLevel::Verbose);
    }

    #[test]
    fn apply_level_produces_canonical_bools() {
        assert_eq!(
            apply_diagnostics_level(DiagnosticsLevel::Off),
            (false, false)
        );
        assert_eq!(
            apply_diagnostics_level(DiagnosticsLevel::Basic),
            (true, false)
        );
        assert_eq!(
            apply_diagnostics_level(DiagnosticsLevel::Verbose),
            (true, true)
        );
    }

    #[test]
    fn round_trip_is_lossless_for_canonical_combos() {
        for (debug, stt_debug) in [(false, false), (true, false), (true, true)] {
            let level = diagnostics_level(debug, stt_debug);
            assert_eq!(
                apply_diagnostics_level(level),
                (debug, stt_debug),
                "round-trip changed ({debug}, {stt_debug})"
            );
        }
    }

    #[test]
    fn impossible_combo_round_trips_into_a_canonical_verbose() {
        // The odd input does not round-trip to itself (by design) but lands on
        // the canonical Verbose bools, which themselves round-trip cleanly.
        let level = diagnostics_level(false, true);
        let bools = apply_diagnostics_level(level);
        assert_eq!(bools, (true, true));
        assert_eq!(
            diagnostics_level(bools.0, bools.1),
            DiagnosticsLevel::Verbose
        );
    }

    #[test]
    fn all_is_ordered_off_basic_verbose() {
        assert_eq!(
            DiagnosticsLevel::ALL,
            [
                DiagnosticsLevel::Off,
                DiagnosticsLevel::Basic,
                DiagnosticsLevel::Verbose
            ]
        );
    }
}
