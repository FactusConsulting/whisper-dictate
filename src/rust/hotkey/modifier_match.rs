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

/// Sentinel value emitted by every `[chord]` / `[rdev/callback]` /
/// `[hotkey/rdev]` diagnostic line for a key name that is NOT
/// PTT-eligible. Kept as a module constant so tests can pin the exact
/// string and so a future rename (e.g. `<hidden>`, `<non-ptt>`) is a
/// single edit.
pub const REDACTED_KEY_NAME: &str = "<redacted>";

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
        // `win` / `win_l` / `win_r` are Windows-terminology aliases for the
        // Meta / Super key family that rdev emits as `cmd_l` / `cmd_r`.
        // `settings_schema.json` advertises the win_* names, but until we
        // accepted them as `cmd` family here a binding of `win_l+f9` never
        // matched any rdev press (`modifier_family("win_l")` was `None`, so
        // `modifier_matches` fell through to `pressed == target` and every
        // real `cmd_l` press missed). Codex P2 #656 discussion r3663653258.
        "cmd" | "cmd_l" | "cmd_r" | "win" | "win_l" | "win_r" => Some("cmd"),
        _ => None,
    }
}

/// The set of bare-modifier names (no side) — a press carrying one of these
/// is a sideless event whose side the OS did not report.
fn is_generic_modifier(name: &str) -> bool {
    matches!(name, "ctrl" | "shift" | "alt" | "cmd" | "win")
}

/// `alt_gr`, `right_alt`, and `ralt` are all the same physical key on every
/// supported layout; canonicalise every alias to `alt_r` for side comparisons
/// so a binding captured as one form matches a press delivered as another
/// (P2 #346 finding 4).
pub fn canonical_side(name: &str) -> &str {
    match name {
        "alt_gr" | "right_alt" | "ralt" => "alt_r",
        // Windows-terminology aliases: normalise to the `cmd_*` side names
        // rdev actually emits (`K::MetaLeft` → `"cmd_l"`, `K::MetaRight` →
        // `"cmd_r"`) so a `win_l` target and a `cmd_l` press canonicalise
        // to the same side and match. Codex P2 #656 r3663653258.
        "win_l" => "cmd_l",
        "win_r" => "cmd_r",
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

/// Redact `name` for a hotkey-diagnostic log line so ordinary desktop
/// typing (letters, digits, punctuation, synthetic `__rdev_<Debug>`
/// names) never lands verbatim in the diagnostic log. A key is
/// considered "PTT-eligible for diagnostics" and passes through when
/// it names a modifier alias ([`modifier_family`] is `Some`) or a
/// non-modifier that can legitimately appear in a PTT chord (F-keys,
/// `space`, `esc`, `tab`, `enter`, `pause`). Everything else — the
/// letter/digit/punctuation stream a user types into other apps —
/// renders as [`REDACTED_KEY_NAME`].
///
/// The predicate is a superset of `rdev_driver::is_rdev_supported_name`
/// (which is `#[cfg(feature = "rust-hotkeys")]`, so unreachable from
/// the always-compiled [`tracker`]/[`modifier_match`] path). Deliberately
/// includes `pause`, which both the RegisterHotKey and rdev backends can
/// deliver as a trigger.
///
/// Codex P1 #665 discussion r3663766123 + P1 #665 discussion
/// PRRT_kwDOSfNjQs6UXh5C: the earlier P1 fix on the `[rdev/callback]`
/// pre-filter trace was undone downstream because `raw_from_rdev`
/// preserves the raw key identity as `__rdev_KeyA` for unmapped keys
/// and the tracker's `[chord]` line then logged it verbatim; the
/// callers now route both surfaces through this helper.
#[must_use]
pub fn redact_key_name_for_diag(name: &str) -> &str {
    if is_ptt_diag_eligible(name) {
        name
    } else {
        REDACTED_KEY_NAME
    }
}

/// True iff `name` names a key that can appear in a PTT chord and
/// therefore carries diagnostic value in a `[chord]` / `[rdev/callback]`
/// trace. Split out so the redactor and any future
/// `is_pass_through_name` sibling stay in one place.
fn is_ptt_diag_eligible(name: &str) -> bool {
    if modifier_family(name).is_some() {
        return true;
    }
    matches!(
        name,
        "f1" | "f2"
            | "f3"
            | "f4"
            | "f5"
            | "f6"
            | "f7"
            | "f8"
            | "f9"
            | "f10"
            | "f11"
            | "f12"
            | "space"
            | "esc"
            | "tab"
            | "enter"
            | "pause"
    )
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

    // -----------------------------------------------------------------------
    // Codex P2 #656 r3663653258 — win_* aliases (Windows-key family).
    //
    // `settings_schema.json` advertises the win_* names, but rdev emits
    // `cmd_l`/`cmd_r` for the physical Meta/Super keys. Without treating
    // win_* as `cmd`-family aliases a `win_l+f9` binding parsed by the
    // supervisor (RegisterHotKey side-specific rejection → rdev fallback)
    // would never fire because `modifier_matches` fell through to plain
    // equality (`"cmd_l" == "win_l"`).
    // -----------------------------------------------------------------------

    #[test]
    fn win_aliases_share_cmd_family() {
        assert_eq!(modifier_family("win"), Some("cmd"));
        assert_eq!(modifier_family("win_l"), Some("cmd"));
        assert_eq!(modifier_family("win_r"), Some("cmd"));
    }

    #[test]
    fn win_side_specific_matches_cmd_press_from_rdev() {
        // rdev delivers `cmd_l` / `cmd_r` for the physical Windows key;
        // a user-configured `win_l` / `win_r` target must accept them.
        assert!(modifier_matches("cmd_l", "win_l"));
        assert!(modifier_matches("cmd_r", "win_r"));
        // ...and the reverse (a synthetic `win_l` press satisfies a
        // configured `cmd_l` target) — same physical key.
        assert!(modifier_matches("win_l", "cmd_l"));
        assert!(modifier_matches("win_r", "cmd_r"));
    }

    #[test]
    fn win_side_specific_rejects_opposite_side() {
        // Same rule as ctrl/shift/alt: a sided win binding must not fire
        // on the opposite physical side.
        assert!(!modifier_matches("cmd_r", "win_l"));
        assert!(!modifier_matches("cmd_l", "win_r"));
        assert!(!modifier_matches("win_r", "win_l"));
    }

    #[test]
    fn generic_win_matches_any_cmd_press() {
        // A user who binds bare `win` (side-insensitive) must have both
        // physical sides satisfy it, and the rdev-emitted `cmd_l`/`cmd_r`
        // must pass.
        assert!(modifier_matches("cmd_l", "win"));
        assert!(modifier_matches("cmd_r", "win"));
        assert!(modifier_matches("win_l", "win"));
        assert!(modifier_matches("win_r", "win"));
        // ... and the generic-fallback branch: a sideless `win` / `cmd`
        // press satisfies a side-specific `win_l` target (chord still
        // starts when the OS did not report a side).
        assert!(modifier_matches("win", "win_l"));
        assert!(modifier_matches("cmd", "win_l"));
    }

    #[test]
    fn win_l_and_cmd_l_share_a_canonical_side() {
        // `all_targets_have_distinct_match` uses `canonical_side` when
        // comparing "same physical key". A binding of `win_l+cmd_l+f9`
        // should NOT succeed on a single physical Windows-key press
        // (they're the same key), just like `ctrl_l+ctrl_l+f9` wouldn't.
        assert_eq!(canonical_side("win_l"), canonical_side("cmd_l"));
        assert_eq!(canonical_side("win_r"), canonical_side("cmd_r"));
    }

    // -----------------------------------------------------------------------
    // Codex P1 (PR #665 review) — `redact_key_name_for_diag` predicate.
    //
    // The pre-filter trace redactor added earlier only covered
    // `[rdev/callback]`; the tracker's `[chord]` line then logged the
    // synthetic `__rdev_KeyA` name for every non-PTT keystroke, defeating
    // the redaction. This predicate is the single source of truth both
    // surfaces now route through — pin its behaviour so a future tweak
    // (e.g. adding a bare-modifier alias to `modifier_family`) cannot
    // silently narrow the redaction.
    // -----------------------------------------------------------------------

    #[test]
    fn redact_hides_ordinary_typing_and_synthetic_names() {
        // The exact identity-bearing shapes `raw_from_rdev` produces for
        // unmapped keys — plus the literal ascii names for keys the OS
        // reports directly. Every one must be redacted so a debug/trace
        // log window cannot reconstruct password / token fragments.
        for name in [
            "__rdev_KeyA",
            "__rdev_Num5",
            "__rdev_Semicolon",
            "__rdev_KeyE",
            "__rdev_Slash",
            "__rdev_KpMinus",
            "a",
            "5",
            ";",
            "hyphen",
            "period",
            "backspace",
            "left",
            "up",
        ] {
            assert_eq!(
                redact_key_name_for_diag(name),
                REDACTED_KEY_NAME,
                "non-PTT name {name:?} must be redacted",
            );
        }
    }

    #[test]
    fn redact_keeps_ptt_eligible_names_visible() {
        // The chord-matcher trace's whole diagnostic value is spotting
        // cases like "held includes `ctrl` but binding is `ctrl_l`" — so
        // modifier aliases and PTT-eligible triggers MUST survive
        // redaction verbatim. Also includes `pause` (RegisterHotKey
        // trigger) and every modifier alias family.
        for name in [
            "ctrl",
            "ctrl_l",
            "ctrl_r",
            "shift",
            "shift_l",
            "shift_r",
            "alt",
            "alt_l",
            "alt_r",
            "alt_gr",
            "right_alt",
            "ralt",
            "cmd",
            "cmd_l",
            "cmd_r",
            "win",
            "win_l",
            "win_r",
            "f1",
            "f9",
            "f12",
            "space",
            "esc",
            "tab",
            "enter",
            "pause",
        ] {
            assert_eq!(
                redact_key_name_for_diag(name),
                name,
                "PTT-eligible name {name:?} must survive redaction verbatim",
            );
        }
    }
}
