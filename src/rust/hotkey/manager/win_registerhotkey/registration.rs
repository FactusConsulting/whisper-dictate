//! registration policy for the Windows RegisterHotKey driver.

use super::{parse_chord, ParsedChord};

/// The outcome of pre-validating a `Register` command before any OS
/// state changes — the "validate BEFORE unregister" contract, unit
/// tested directly.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RegisterPlan {
    /// Parse succeeded. Caller should unregister the current chord,
    /// then install `chord` via `RegisterHotKey`.
    Install(ParsedChord),
    /// Parse failed. Caller MUST leave the previous binding intact
    /// (no `UnregisterHotKey`) and ack the error back — otherwise a
    /// rebind attempt with a bad chord leaves the process without any
    /// listener at all.
    Reject(String),
}

/// Pure planner: parse the target chord and translate the result into
/// the plan `handle_command` executes. No OS calls; no state mutation.
pub(crate) fn plan_register(targets: &[String]) -> RegisterPlan {
    match parse_chord(targets) {
        Ok(chord) => RegisterPlan::Install(chord),
        Err(msg) => RegisterPlan::Reject(msg),
    }
}
