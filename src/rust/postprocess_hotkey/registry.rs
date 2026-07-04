//! A cycle-through-able list of [`PostprocessProfile`]s.
//!
//! The second hotkey (issue #319) has two duties:
//!
//! 1. **Fire** the currently-active profile against the last dictated text
//!    (see [`super::dispatch`]).
//! 2. **Cycle** through the configured profiles when the user chords it
//!    with a modifier — the registry owns the wrap-around bookkeeping so
//!    the caller only has to say "advance to the next one".
//!
//! Persisting the active index between runs is up to the caller
//! (`postprocess_profile_index` in the config schema). The registry is
//! pure logic so it stays trivially unit-testable without touching disk.

use serde::{Deserialize, Serialize};

use crate::postprocess_hotkey::profile::{built_in_profiles, PostprocessProfile};

/// Errors returned by [`ProfileRegistry`] operations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// The registry was constructed with no profiles. Cycle / activate /
    /// current all fail until a caller loads at least one profile.
    #[error("no postprocess profiles configured")]
    Empty,
    /// The requested index is out of range for the current profile list.
    #[error("profile index {index} out of range (have {len})")]
    IndexOutOfRange { index: usize, len: usize },
}

/// Owning collection of profiles with a tracked active index.
///
/// Construction never mutates the input list — the caller decides the
/// ordering. `active` is clamped into range on `new` so a stale index
/// from a config where the user removed profiles does not panic on the
/// first press; instead it wraps to `0`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileRegistry {
    profiles: Vec<PostprocessProfile>,
    active: usize,
}

impl ProfileRegistry {
    /// Build a registry from an explicit profile list and the persisted
    /// active index. An out-of-range `active` (e.g. the user shrank the
    /// list between runs) is clamped to `0` so the next press still
    /// fires something reasonable.
    pub fn new(profiles: Vec<PostprocessProfile>, active: usize) -> Self {
        let clamped = if profiles.is_empty() {
            0
        } else if active >= profiles.len() {
            0
        } else {
            active
        };
        Self {
            profiles,
            active: clamped,
        }
    }

    /// Convenience for first-run: seed the built-in defaults with the
    /// grammar-fix profile active. Same list as
    /// [`built_in_profiles`].
    pub fn built_in() -> Self {
        Self::new(built_in_profiles(), 0)
    }

    /// Number of profiles the registry holds.
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// True when the registry has no profiles at all.
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// Index of the currently active profile.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Borrow the currently active profile, or [`RegistryError::Empty`]
    /// when the list is empty.
    pub fn current(&self) -> Result<&PostprocessProfile, RegistryError> {
        self.profiles.get(self.active).ok_or(RegistryError::Empty)
    }

    /// Immutable view of the full profile list.
    pub fn profiles(&self) -> &[PostprocessProfile] {
        &self.profiles
    }

    /// Advance the active index by one (wrapping). Returns the new active
    /// index, or [`RegistryError::Empty`] if there is nothing to cycle
    /// through.
    pub fn cycle_next(&mut self) -> Result<usize, RegistryError> {
        if self.profiles.is_empty() {
            return Err(RegistryError::Empty);
        }
        self.active = (self.active + 1) % self.profiles.len();
        Ok(self.active)
    }

    /// Cycle the other way — useful when the user chords the second
    /// hotkey with Shift.
    pub fn cycle_previous(&mut self) -> Result<usize, RegistryError> {
        if self.profiles.is_empty() {
            return Err(RegistryError::Empty);
        }
        // `len - 1 - active` moves left; wrapping via modular arithmetic
        // keeps the same "always in-range" invariant as `cycle_next`.
        self.active = (self.active + self.profiles.len() - 1) % self.profiles.len();
        Ok(self.active)
    }

    /// Activate a specific index. Errors when the index is out of range
    /// so a bogus config value is loud rather than silently wrapping.
    pub fn activate(&mut self, index: usize) -> Result<usize, RegistryError> {
        if index >= self.profiles.len() {
            return Err(RegistryError::IndexOutOfRange {
                index,
                len: self.profiles.len(),
            });
        }
        self.active = index;
        Ok(self.active)
    }

    /// Look up a profile by its display name (case-insensitive, trimmed).
    /// Returns `Some(index)` for the FIRST match, `None` when nothing
    /// matches. Callers use this to jump to "the profile named foo"
    /// from a settings UI without hard-coding indices.
    pub fn find_by_name(&self, needle: &str) -> Option<usize> {
        let target = needle.trim().to_ascii_lowercase();
        if target.is_empty() {
            return None;
        }
        self.profiles.iter().position(|p| {
            p.display_name()
                .trim()
                .to_ascii_lowercase()
                .eq_ignore_ascii_case(&target)
        })
    }

    /// Parse a JSON array of profile objects into a registry. Used to
    /// load the `postprocess_profiles` config setting into the runtime
    /// state. Empty / whitespace-only input yields an empty registry
    /// so the caller can then fall back to [`ProfileRegistry::built_in`]
    /// without special-casing the empty state twice.
    pub fn from_json(raw: &str, active: usize) -> Result<Self, serde_json::Error> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(Self::new(Vec::new(), 0));
        }
        let profiles: Vec<PostprocessProfile> = serde_json::from_str(trimmed)?;
        Ok(Self::new(profiles, active))
    }

    /// Serialise the profile list as JSON so it round-trips through the
    /// `postprocess_profiles` string setting.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.profiles)
    }
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self::built_in()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profiles() -> Vec<PostprocessProfile> {
        vec![
            PostprocessProfile {
                name: "A".to_owned(),
                mode: "clean".to_owned(),
                ..PostprocessProfile::default()
            },
            PostprocessProfile {
                name: "B".to_owned(),
                mode: "email".to_owned(),
                ..PostprocessProfile::default()
            },
            PostprocessProfile {
                name: "C".to_owned(),
                mode: "bullets".to_owned(),
                ..PostprocessProfile::default()
            },
        ]
    }

    #[test]
    fn new_clamps_stale_active_index_to_zero() {
        let reg = ProfileRegistry::new(sample_profiles(), 99);
        assert_eq!(reg.active_index(), 0);
        assert_eq!(reg.current().unwrap().name, "A");
    }

    #[test]
    fn empty_registry_reports_errors_on_all_ops() {
        let mut reg = ProfileRegistry::new(Vec::new(), 0);
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert_eq!(reg.current(), Err(RegistryError::Empty));
        assert_eq!(reg.cycle_next(), Err(RegistryError::Empty));
        assert_eq!(reg.cycle_previous(), Err(RegistryError::Empty));
    }

    #[test]
    fn cycle_next_wraps_around_and_returns_new_index() {
        let mut reg = ProfileRegistry::new(sample_profiles(), 0);
        assert_eq!(reg.cycle_next().unwrap(), 1);
        assert_eq!(reg.cycle_next().unwrap(), 2);
        assert_eq!(reg.cycle_next().unwrap(), 0);
        assert_eq!(reg.current().unwrap().name, "A");
    }

    #[test]
    fn cycle_previous_wraps_around_the_other_way() {
        let mut reg = ProfileRegistry::new(sample_profiles(), 0);
        assert_eq!(reg.cycle_previous().unwrap(), 2);
        assert_eq!(reg.cycle_previous().unwrap(), 1);
        assert_eq!(reg.cycle_previous().unwrap(), 0);
    }

    #[test]
    fn activate_rejects_out_of_range_index() {
        let mut reg = ProfileRegistry::new(sample_profiles(), 0);
        let err = reg.activate(9).unwrap_err();
        assert_eq!(err, RegistryError::IndexOutOfRange { index: 9, len: 3 });
        // The active index must not change on a failed activate.
        assert_eq!(reg.active_index(), 0);
    }

    #[test]
    fn activate_accepts_boundary_index() {
        let mut reg = ProfileRegistry::new(sample_profiles(), 0);
        assert_eq!(reg.activate(2).unwrap(), 2);
        assert_eq!(reg.current().unwrap().name, "C");
    }

    #[test]
    fn find_by_name_matches_case_insensitively() {
        let reg = ProfileRegistry::new(sample_profiles(), 0);
        assert_eq!(reg.find_by_name(" a "), Some(0));
        assert_eq!(reg.find_by_name("B"), Some(1));
        assert_eq!(reg.find_by_name("nope"), None);
        assert_eq!(reg.find_by_name("   "), None);
    }

    #[test]
    fn from_json_empty_input_yields_empty_registry() {
        let reg = ProfileRegistry::from_json("", 0).unwrap();
        assert!(reg.is_empty());
        let reg = ProfileRegistry::from_json("   ", 0).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn json_round_trip_preserves_profiles() {
        let reg = ProfileRegistry::new(sample_profiles(), 1);
        let json = reg.to_json().unwrap();
        let round = ProfileRegistry::from_json(&json, reg.active_index()).unwrap();
        assert_eq!(round.profiles(), reg.profiles());
        assert_eq!(round.active_index(), reg.active_index());
    }

    #[test]
    fn built_in_registry_defaults_to_grammar_active() {
        let reg = ProfileRegistry::built_in();
        assert!(!reg.is_empty());
        assert_eq!(reg.active_index(), 0);
        assert_eq!(reg.current().unwrap().mode, "clean");
    }
}
