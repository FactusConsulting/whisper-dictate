//! Pure mapping between the three persisted debug bools (`debug`, `stt_debug`,
//! `trace`) and a single user-facing **Diagnostics level** (Off / Basic /
//! Verbose / Trace).
//!
//! The Output tab used to expose the raw env-named toggles directly, which read
//! as confusing low-level switches. They are kept untouched in the config, the
//! env vars and the worker — this module is a *pure UI affordance* that presents
//! them as one ordered "how much output" dropdown.
//!
//! Mapping (see [`diagnostics_level`] / [`apply_diagnostics_level`]):
//!
//! | Level   | `debug` | `stt_debug` | `trace` | meaning                          |
//! |---------|---------|-------------|---------|----------------------------------|
//! | Off     | false   | false       | false   | no diagnostics                   |
//! | Basic   | true    | false       | false   | concise per-utterance `[health]` |
//! | Verbose | true    | true        | false   | + config dump + per-segment STT  |
//! | Trace   | true    | true        | true    | + full audio device/capture log  |
//!
//! Basic (`debug:on`) emits one plain-language `[health]` line per utterance
//! (mic level/SNR + model confidence + terse warnings). Verbose adds the startup
//! effective-settings dump and the per-segment STT/dictionary detail on top.
//! Trace adds the full audio-device enumeration at startup plus a line for every
//! capture-open *attempt* (host-api × rate × channels × dtype × auto_convert), so
//! a "why won't my mic open" is diagnosable from the log alone.
//!
//! `trace` always implies at least Verbose (it sits on top of the config dump +
//! per-segment detail), and `stt_debug` always implies at least Verbose, so the
//! "impossible" hand-edited combos (any `trace`, or `stt_debug` without `debug`)
//! are bucketed at the most informative coherent level rather than dropping the
//! extra detail.

/// Ordered diagnostics verbosity surfaced by the System-tab dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum DiagnosticsLevel {
    /// No diagnostics: `debug:off, stt_debug:off, trace:off`.
    Off,
    /// Concise per-utterance `[health]` line: `debug:on, stt_debug:off`.
    Basic,
    /// Health line + startup config dump + per-segment STT/dictionary detail:
    /// `debug:on, stt_debug:on, trace:off`.
    Verbose,
    /// Verbose plus full audio-device enumeration and every capture-open attempt:
    /// `debug:on, stt_debug:on, trace:on`.
    Trace,
}

impl DiagnosticsLevel {
    /// Off → Basic → Verbose → Trace, ascending, for rendering the dropdown.
    pub(in crate::ui) const ALL: [DiagnosticsLevel; 4] = [
        DiagnosticsLevel::Off,
        DiagnosticsLevel::Basic,
        DiagnosticsLevel::Verbose,
        DiagnosticsLevel::Trace,
    ];
}

/// Derive the user-facing level from the three persisted bools.
///
/// `trace` always implies the most-informative level (it sits on top of the
/// Verbose config dump + per-segment detail), and `stt_debug` always implies at
/// least Verbose, so the impossible hand-edited combos land on a coherent value
/// rather than dropping the extra detail.
pub(in crate::ui) fn diagnostics_level(
    debug: bool,
    stt_debug: bool,
    trace: bool,
) -> DiagnosticsLevel {
    match (debug, stt_debug, trace) {
        (_, _, true) => DiagnosticsLevel::Trace,
        (_, true, false) => DiagnosticsLevel::Verbose,
        (true, false, false) => DiagnosticsLevel::Basic,
        (false, false, false) => DiagnosticsLevel::Off,
    }
}

/// Expand a chosen level back into the `(debug, stt_debug, trace)` bools to
/// persist.
pub(in crate::ui) fn apply_diagnostics_level(level: DiagnosticsLevel) -> (bool, bool, bool) {
    match level {
        DiagnosticsLevel::Off => (false, false, false),
        DiagnosticsLevel::Basic => (true, false, false),
        DiagnosticsLevel::Verbose => (true, true, false),
        DiagnosticsLevel::Trace => (true, true, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_from_bools_canonical_combos() {
        assert_eq!(
            diagnostics_level(false, false, false),
            DiagnosticsLevel::Off
        );
        assert_eq!(
            diagnostics_level(true, false, false),
            DiagnosticsLevel::Basic
        );
        assert_eq!(
            diagnostics_level(true, true, false),
            DiagnosticsLevel::Verbose
        );
        assert_eq!(diagnostics_level(true, true, true), DiagnosticsLevel::Trace);
    }

    #[test]
    fn impossible_combos_are_bucketed_at_the_most_informative_level() {
        // debug:off + stt_debug:on can only come from a hand-edited config; any
        // stt_debug implies at least Verbose so the dropdown stays coherent.
        assert_eq!(
            diagnostics_level(false, true, false),
            DiagnosticsLevel::Verbose
        );
        // trace:on without the lower bools still reads as Trace (the maximal,
        // most-informative coherent value).
        assert_eq!(
            diagnostics_level(false, false, true),
            DiagnosticsLevel::Trace
        );
        assert_eq!(
            diagnostics_level(true, false, true),
            DiagnosticsLevel::Trace
        );
    }

    #[test]
    fn apply_level_produces_canonical_bools() {
        // The four canonical (debug, stt_debug, trace) combinations the dropdown
        // can ever write — exactly the spec'd Off/Basic/Verbose/Trace mapping.
        assert_eq!(
            apply_diagnostics_level(DiagnosticsLevel::Off),
            (false, false, false)
        );
        assert_eq!(
            apply_diagnostics_level(DiagnosticsLevel::Basic),
            (true, false, false)
        );
        assert_eq!(
            apply_diagnostics_level(DiagnosticsLevel::Verbose),
            (true, true, false)
        );
        assert_eq!(
            apply_diagnostics_level(DiagnosticsLevel::Trace),
            (true, true, true)
        );
    }

    #[test]
    fn round_trip_is_lossless_for_canonical_combos() {
        for (debug, stt_debug, trace) in [
            (false, false, false),
            (true, false, false),
            (true, true, false),
            (true, true, true),
        ] {
            let level = diagnostics_level(debug, stt_debug, trace);
            assert_eq!(
                apply_diagnostics_level(level),
                (debug, stt_debug, trace),
                "round-trip changed ({debug}, {stt_debug}, {trace})"
            );
        }
    }

    #[test]
    fn impossible_combo_round_trips_into_a_canonical_level() {
        // The odd input does not round-trip to itself (by design) but lands on
        // canonical bools which themselves round-trip cleanly.
        let level = diagnostics_level(false, true, false);
        let bools = apply_diagnostics_level(level);
        assert_eq!(bools, (true, true, false));
        assert_eq!(
            diagnostics_level(bools.0, bools.1, bools.2),
            DiagnosticsLevel::Verbose
        );
    }

    #[test]
    fn all_is_ordered_off_basic_verbose_trace() {
        assert_eq!(
            DiagnosticsLevel::ALL,
            [
                DiagnosticsLevel::Off,
                DiagnosticsLevel::Basic,
                DiagnosticsLevel::Verbose,
                DiagnosticsLevel::Trace,
            ]
        );
    }
}
