//! Side-aware modifier matching for the Rust PTT coordinator.
//!
//! Mirrors [`vp_keys_solo.modifier_matches`](../../../python/whisper_dictate/vp_keys_solo.py)
//! so the Rust hotkey backend (issue #318) reproduces the side-specific +
//! generic-fallback semantics that ship today on the Python listener. The
//! single predicate every matching site routes through ([`modifier_matches`])
//! reverses the full side-insensitivity of #254 (left and right modifiers are
//! distinct) while keeping a GENERIC fallback so reliability is preserved when
//! the OS reports a sideless modifier press.
//!
//! Matching rules (verbatim from the Python doctring, restated here so the
//! Rust port is auditable in isolation):
//!
//! * **Side-specific target** (`ctrl_l`): satisfied by the SAME specific side
//!   (`ControlLeft`) OR by the GENERIC family press (a synthetic family token
//!   the host may emit when it cannot decide a side) — a fail-safe so the
//!   chord still starts if a sideless modifier slips through. NOT satisfied
//!   by the OPPOSITE specific side (`ControlRight`).
//! * **Generic target** (bare `ctrl` — only when the user binds a sideless
//!   modifier): matches ANY variant of that family, i.e. side-insensitive
//!   within the family.
//! * **Non-modifier target** (`f9`, `space`, the `esc` quit key): plain name
//!   equality — unchanged behaviour.
//!
//! Residual reliability tradeoff (documented for the user): with a
//! side-specific binding, (a) if the OS delivers the OPPOSITE specific side
//! the chord will NOT match (rare; the accepted cost of side-specificity),
//! and (b) a press of the other side that the OS happens to deliver AS the
//! generic family token WILL match (rare leak, fail-safe toward starting).

use std::collections::HashSet;

/// Modifier family token for a given key NAME, or `None` for non-modifiers.
///
/// Names use the same lowercase convention as the PTT setting strings
/// (`ctrl_l`, `shift_r`, `alt_gr`, `cmd_l`, ...), so a press normalised via
/// [`canonicalise_key_name`] can be compared directly against a target name
/// from settings.
pub fn modifier_family(name: &str) -> Option<&'static str> {
    match name {
        "ctrl" | "ctrl_l" | "ctrl_r" => Some("ctrl"),
        "shift" | "shift_l" | "shift_r" => Some("shift"),
        // `right_alt` / `ralt` are accepted aliases for `alt_gr` / `alt_r`
        // (P2 #346 finding 4): some users and documentation use these names.
        "alt" | "alt_l" | "alt_r" | "alt_gr" | "right_alt" | "ralt" => Some("alt"),
        // `super_l` / `super_r` are the Linux names for the Meta / Win key, i.e.
        // the SAME physical key rdev/macOS call `cmd`. The old evdev backend
        // accepted them, so alias them into the cmd family (mirrors the alt_gr
        // alias above) — otherwise a saved `super_l` binding would be rejected
        // as UnsupportedKey and PTT would stay dead on Wayland (Codex #462 P2).
        "cmd" | "cmd_l" | "cmd_r" | "super_l" | "super_r" => Some("cmd"),
        _ => None,
    }
}

/// The set of bare-modifier names (no side) — a press carrying one of these
/// is a sideless event whose side the OS did not report.
fn is_generic_modifier(name: &str) -> bool {
    matches!(name, "ctrl" | "shift" | "alt" | "cmd")
}

/// Canonicalise side-aliases so a binding captured as one name matches a press
/// delivered as another:
/// * `alt_gr` / `right_alt` / `ralt` → `alt_r` (same physical key; P2 #346).
/// * `super_l` / `super_r` → `cmd_l` / `cmd_r` — the Meta / Win key, which rdev
///   and evdev's [`super::manager`] name `cmd_*`; aliasing lets a `super_l`
///   target match a `cmd_l` press (Codex #462 P2).
pub fn canonical_side(name: &str) -> &str {
    match name {
        "alt_gr" | "right_alt" | "ralt" => "alt_r",
        "super_l" => "cmd_l",
        "super_r" => "cmd_r",
        other => other,
    }
}

/// Side-aware match: does a press named `pressed` satisfy the binding named
/// `target`?
///
/// `pressed` is the name of a real key event (already normalised — see the
/// `rdev` ↔ name table in [`crate::hotkey::manager`]); `target` is the PTT
/// `key` setting name for one chord member (`"ctrl_l"`, `"ctrl_r"`, the
/// generic `"ctrl"`, `"f9"`, ...).
pub fn modifier_matches(pressed: &str, target: &str) -> bool {
    let Some(target_family) = modifier_family(target) else {
        // Non-modifier target (`f9`, `space`, a letter, `esc`): exact name
        // equality, no fancy side logic.
        return pressed == target;
    };
    let Some(pressed_family) = modifier_family(pressed) else {
        return false;
    };
    if pressed_family != target_family {
        return false; // different modifier family — never matches
    }
    if is_generic_modifier(target) {
        // Generic target: any side / generic press of the family.
        return true;
    }
    // Side-specific target: same side (alt_gr ≡ alt_r) OR the generic family
    // press (side unknown → fail-safe match). The opposite side fails.
    canonical_side(pressed) == canonical_side(target) || is_generic_modifier(pressed)
}

/// True iff every `target` name can be paired with a DISTINCT `held` key that
/// matches it side-aware — a 1:1 (injective) assignment over the bipartite
/// "names × held" graph.
///
/// Why not the naive `all(any(modifier_matches(...)))`: the generic fallback
/// means a single held `ctrl` matches BOTH `ctrl_l` and `ctrl_r`, so the naive
/// form would declare a `ctrl_l+ctrl_r` both-sides binding complete on ONE
/// physical Ctrl. Requiring a distinct held key per target enforces the real
/// semantics: an N-key chord needs N held keys. Chord sizes are tiny, so a
/// plain augmenting-path (Kuhn) matching is far more than fast enough.
pub fn all_targets_have_distinct_match(targets: &[String], held: &HashSet<String>) -> bool {
    if held.len() < targets.len() {
        return false;
    }
    let held_vec: Vec<&String> = held.iter().collect();
    let mut assigned: Vec<Option<usize>> = vec![None; held_vec.len()];

    fn augment(
        t_idx: usize,
        targets: &[String],
        held_vec: &[&String],
        assigned: &mut Vec<Option<usize>>,
        visited: &mut HashSet<usize>,
    ) -> bool {
        for (h_idx, hk) in held_vec.iter().enumerate() {
            if visited.contains(&h_idx) || !modifier_matches(hk, &targets[t_idx]) {
                continue;
            }
            visited.insert(h_idx);
            let prev = assigned[h_idx];
            if prev.is_none() || augment(prev.unwrap(), targets, held_vec, assigned, visited) {
                assigned[h_idx] = Some(t_idx);
                return true;
            }
        }
        false
    }

    (0..targets.len()).all(|t_idx| {
        let mut visited = HashSet::new();
        augment(t_idx, targets, &held_vec, &mut assigned, &mut visited)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    // -- modifier_matches ----------------------------------------------------

    #[test]
    fn side_specific_target_matches_same_side() {
        assert!(modifier_matches("ctrl_l", "ctrl_l"));
        assert!(modifier_matches("shift_r", "shift_r"));
    }

    #[test]
    fn side_specific_target_rejects_opposite_side() {
        assert!(!modifier_matches("ctrl_r", "ctrl_l"));
        assert!(!modifier_matches("shift_l", "shift_r"));
        assert!(!modifier_matches("alt_l", "alt_r"));
    }

    #[test]
    fn side_specific_target_accepts_generic_fallback() {
        // OS delivers a sideless ctrl press — must still satisfy ctrl_l so
        // the chord starts (fail-safe). Mirrors Python's generic-fallback
        // branch (vp_keys_solo.modifier_matches).
        assert!(modifier_matches("ctrl", "ctrl_l"));
        assert!(modifier_matches("ctrl", "ctrl_r"));
        assert!(modifier_matches("shift", "shift_l"));
        assert!(modifier_matches("alt", "alt_r"));
    }

    #[test]
    fn generic_target_matches_any_side() {
        // User explicitly bound a sideless `ctrl` → either physical side
        // satisfies it.
        assert!(modifier_matches("ctrl_l", "ctrl"));
        assert!(modifier_matches("ctrl_r", "ctrl"));
        assert!(modifier_matches("ctrl", "ctrl"));
    }

    #[test]
    fn different_families_never_match() {
        assert!(!modifier_matches("shift_l", "ctrl_l"));
        assert!(!modifier_matches("alt_r", "shift_r"));
        assert!(!modifier_matches("cmd_l", "ctrl_l"));
    }

    #[test]
    fn alt_gr_is_canonical_alt_r() {
        assert!(modifier_matches("alt_gr", "alt_r"));
        assert!(modifier_matches("alt_r", "alt_gr"));
    }

    #[test]
    fn super_is_canonical_cmd() {
        // Codex #462 P2: both backends emit the Meta/Win key as `cmd_*`, so a
        // saved `super_*` binding must match the `cmd_*` press (and vice versa),
        // and must NOT match the opposite side.
        assert!(modifier_matches("cmd_l", "super_l"));
        assert!(modifier_matches("super_l", "cmd_l"));
        assert!(modifier_matches("cmd_r", "super_r"));
        assert!(!modifier_matches("cmd_r", "super_l"));
        // Generic cmd press (side unknown) still satisfies a super_l target.
        assert!(modifier_matches("cmd", "super_l"));
        // Different family stays a non-match.
        assert!(!modifier_matches("ctrl_l", "super_l"));
    }

    #[test]
    fn non_modifier_target_uses_plain_equality() {
        assert!(modifier_matches("f9", "f9"));
        assert!(!modifier_matches("f10", "f9"));
        assert!(modifier_matches("esc", "esc"));
        assert!(!modifier_matches("ctrl_l", "f9"));
    }

    // -- all_targets_have_distinct_match -------------------------------------

    #[test]
    fn single_target_matches_single_held() {
        let names = vec!["ctrl_l".to_string()];
        assert!(all_targets_have_distinct_match(&names, &held(&["ctrl_l"])));
        // generic fallback still satisfies a side-specific target
        assert!(all_targets_have_distinct_match(&names, &held(&["ctrl"])));
        // opposite side does not
        assert!(!all_targets_have_distinct_match(&names, &held(&["ctrl_r"])));
    }

    #[test]
    fn both_sides_chord_needs_two_distinct_held() {
        // The "1:1 matching" property: a single generic ctrl press must NOT
        // be enough to complete a left+right chord, otherwise the chord would
        // fire on one physical Ctrl. This is the test the naive implementation
        // would fail.
        let names = vec!["ctrl_l".to_string(), "ctrl_r".to_string()];
        assert!(!all_targets_have_distinct_match(&names, &held(&["ctrl"])));
        assert!(all_targets_have_distinct_match(
            &names,
            &held(&["ctrl_l", "ctrl_r"])
        ));
        // a generic + one specific is enough: the generic covers the missing
        // side as a fail-safe.
        assert!(all_targets_have_distinct_match(
            &names,
            &held(&["ctrl_l", "ctrl"])
        ));
    }

    #[test]
    fn mixed_chord_modifier_plus_function_key() {
        let names = vec!["ctrl_l".to_string(), "f9".to_string()];
        assert!(all_targets_have_distinct_match(
            &names,
            &held(&["ctrl_l", "f9"])
        ));
        assert!(!all_targets_have_distinct_match(
            &names,
            &held(&["ctrl_r", "f9"])
        ));
        // Missing the function key — chord incomplete even with both ctrls.
        assert!(!all_targets_have_distinct_match(
            &names,
            &held(&["ctrl_l", "ctrl_r"])
        ));
    }

    #[test]
    fn insufficient_held_keys_returns_false() {
        let names = vec!["ctrl_l".to_string(), "shift_l".to_string()];
        assert!(!all_targets_have_distinct_match(&names, &held(&["ctrl_l"])));
    }

    #[test]
    fn modifier_family_classification() {
        assert_eq!(modifier_family("ctrl_l"), Some("ctrl"));
        assert_eq!(modifier_family("alt_gr"), Some("alt"));
        assert_eq!(modifier_family("shift"), Some("shift"));
        assert_eq!(modifier_family("f9"), None);
        assert_eq!(modifier_family("a"), None);
    }

    // -----------------------------------------------------------------------
    // P2 #346 finding 4: right_alt / ralt aliases.
    // -----------------------------------------------------------------------

    #[test]
    fn right_alt_ralt_are_same_family_as_alt() {
        assert_eq!(modifier_family("right_alt"), Some("alt"));
        assert_eq!(modifier_family("ralt"), Some("alt"));
    }

    #[test]
    fn right_alt_ralt_canonical_side_is_alt_r() {
        assert_eq!(canonical_side("right_alt"), "alt_r");
        assert_eq!(canonical_side("ralt"), "alt_r");
        // Existing alt_gr still maps correctly.
        assert_eq!(canonical_side("alt_gr"), "alt_r");
    }

    #[test]
    fn right_alt_matches_alt_gr_target_and_vice_versa() {
        // A press reported as "right_alt" must satisfy an "alt_gr" binding
        // (and vice versa) since they are the same physical key.
        assert!(modifier_matches("right_alt", "alt_gr"));
        assert!(modifier_matches("alt_gr", "right_alt"));
        assert!(modifier_matches("ralt", "alt_gr"));
        assert!(modifier_matches("right_alt", "ralt"));
    }

    #[test]
    fn right_alt_does_not_satisfy_alt_l_target() {
        // right_alt / ralt are right-side only; left-Alt binding must not fire.
        assert!(!modifier_matches("right_alt", "alt_l"));
        assert!(!modifier_matches("ralt", "alt_l"));
    }
}
