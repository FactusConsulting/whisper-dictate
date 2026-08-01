//! PipeWire capture configuration.
//!
//!
//! On Linux, capture uses `PIPEWIRE_QUANTUM=2048` when the operator has not
//! supplied a value. Existing non-empty values are preserved; other platforms
//! leave the environment unchanged.

/// Env var PipeWire honours for the client-negotiated quantum.
pub const PIPEWIRE_QUANTUM_ENV: &str = "PIPEWIRE_QUANTUM";

/// Safe default quantum for cpal-driven capture on PipeWire.
///
/// The value is intentionally not a `NonZeroU32` — call sites emit it
/// via `.to_string()` into an env var and never do arithmetic on it.
pub const SAFE_DEFAULT_QUANTUM: u32 = 2048;

/// Decision returned by [`decide_pipewire_quantum`]. Kept as an enum so
/// the unit tests can pin the exact branch taken without inspecting the
/// process env, and so the self-test / debug output can name the reason
/// verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuantumDecision {
    /// The operator already set `PIPEWIRE_QUANTUM` to a non-empty value.
    /// We leave it alone; the string is preserved verbatim so a diagnostic
    /// can echo what the user picked.
    UserOverride(String),
    /// The env var was unset (or empty). We should export
    /// [`SAFE_DEFAULT_QUANTUM`] when no operator value is provided.
    ApplyDefault(u32),
}

impl QuantumDecision {
    /// Stable machine-readable token — mirrors the pattern used by
    /// `FailureStage::as_str` in the injection self-test. The audio
    /// self-test emits this in its JSON report so callers can grep for
    /// "user_override" vs "default_applied" without parsing detail.
    pub fn as_str(&self) -> &'static str {
        match self {
            QuantumDecision::UserOverride(_) => "user_override",
            QuantumDecision::ApplyDefault(_) => "default_applied",
        }
    }

    /// Numeric quantum the decision would produce, for diagnostics.
    /// `UserOverride("garbage")` returns `None` because we deliberately
    /// don't parse arbitrary operator input — the string is preserved
    /// verbatim and passed to PipeWire, which will fail loudly if it's
    /// invalid.
    pub fn quantum(&self) -> Option<u32> {
        match self {
            QuantumDecision::UserOverride(s) => s.trim().parse().ok(),
            QuantumDecision::ApplyDefault(q) => Some(*q),
        }
    }
}

/// Pure decision: given the current value of `PIPEWIRE_QUANTUM` (as an
/// `Option` from `env::var`, where `Err` becomes `None`), decide what to
/// do. Trimmed-empty strings count as unset — an operator who writes
/// `PIPEWIRE_QUANTUM=` in a systemd unit almost certainly meant "clear
/// it", not "use the empty string as a quantum".
///
/// The function is intentionally in-crate `pub` (rather than
/// `pub(crate)`) so the self-test module in `audio::self_test` can call
/// it and report the branch that fired.
pub fn decide_pipewire_quantum(current: Option<&str>) -> QuantumDecision {
    match current {
        Some(v) if !v.trim().is_empty() => QuantumDecision::UserOverride(v.to_owned()),
        _ => QuantumDecision::ApplyDefault(SAFE_DEFAULT_QUANTUM),
    }
}

/// Apply the pure decision by mutating `std::env`. On Linux this is
/// called from the capture path before cpal opens the ALSA/PipeWire
/// backend. On non-Linux the function is compiled out — see the `cfg`
/// gate below — so callers never need a `#[cfg]` at the use site.
///
/// Returns the decision that fired so the caller can log it (or expose
/// it in a self-test report).
#[cfg(target_os = "linux")]
pub fn configure_pipewire_env() -> QuantumDecision {
    let current = std::env::var(PIPEWIRE_QUANTUM_ENV).ok();
    let decision = decide_pipewire_quantum(current.as_deref());
    if let QuantumDecision::ApplyDefault(q) = &decision {
        // SAFETY: setting env vars is inherently non-thread-safe. Callers
        // must invoke `configure_pipewire_env` BEFORE any cpal thread has
        // been spawned; the audio pipeline in `audio::mod` respects this by
        // configuring on the calling thread before spawning the capture
        // worker. A stray parallel call would race, but no other module
        // touches PIPEWIRE_QUANTUM so this is a single-writer invariant.
        std::env::set_var(PIPEWIRE_QUANTUM_ENV, q.to_string());
    }
    decision
}

/// No-op on non-Linux. Returns `UserOverride("")` so the caller sees a
/// distinctive marker in a diagnostic — Windows/macOS don't respect
/// `PIPEWIRE_QUANTUM`, so touching `std::env` would be misleading.
#[cfg(not(target_os = "linux"))]
pub fn configure_pipewire_env() -> QuantumDecision {
    QuantumDecision::UserOverride(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_env_applies_safe_default() {
        let d = decide_pipewire_quantum(None);
        assert_eq!(d, QuantumDecision::ApplyDefault(SAFE_DEFAULT_QUANTUM));
        assert_eq!(d.quantum(), Some(2048));
        assert_eq!(d.as_str(), "default_applied");
    }

    #[test]
    fn empty_string_treated_as_unset() {
        // An operator writing `PIPEWIRE_QUANTUM=` in a systemd unit almost
        // certainly meant to clear it; a bare empty string is not a valid
        // quantum for PipeWire to consume, so falling back to the safe
        // default matches operator intent AND avoids passing garbage
        // downstream.
        assert_eq!(
            decide_pipewire_quantum(Some("")),
            QuantumDecision::ApplyDefault(SAFE_DEFAULT_QUANTUM),
        );
        assert_eq!(
            decide_pipewire_quantum(Some("   ")),
            QuantumDecision::ApplyDefault(SAFE_DEFAULT_QUANTUM),
        );
    }

    #[test]
    fn user_override_is_preserved_verbatim() {
        // Pro-audio setup on 512 samples: we leave it alone (that's the
        // operator's problem to know their hardware can handle it).
        let d = decide_pipewire_quantum(Some("512"));
        assert_eq!(d, QuantumDecision::UserOverride("512".to_owned()));
        assert_eq!(d.quantum(), Some(512));
        assert_eq!(d.as_str(), "user_override");
    }

    #[test]
    fn user_override_non_numeric_still_preserved() {
        // We don't gate the override on parseability — that's PipeWire's
        // job. The decision preserves the raw string, and `.quantum()`
        // returns None so a diagnostic can flag it without our code
        // silently dropping the user's setting.
        let d = decide_pipewire_quantum(Some("garbage"));
        assert_eq!(d, QuantumDecision::UserOverride("garbage".to_owned()));
        assert_eq!(d.quantum(), None);
        assert_eq!(d.as_str(), "user_override");
    }

    #[test]
    fn safe_default_is_2048_samples() {
        // Keep the default stable while allowing an explicit operator value.
        assert_eq!(SAFE_DEFAULT_QUANTUM, 2048);
    }
}
