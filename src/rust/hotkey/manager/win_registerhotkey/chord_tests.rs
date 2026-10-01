use super::*;

#[test]
fn parses_function_key_alone_with_no_modifiers() {
    let parsed = parse_chord(&s(&["f9"])).expect("f9 parses");
    assert_eq!(parsed.mods, 0);
    assert_eq!(parsed.vk, 0x78);
    assert_eq!(parsed.trigger_name, "f9");
    assert_eq!(parsed.display, "f9");
}

#[test]
fn parses_ctrl_plus_function_key() {
    // The most common Windows-friendly chord: one generic modifier + one
    // trigger. Side-specific `ctrl_l` is rejected (see
    // `rejects_side_specific_modifier_aliases` below) because the OS
    // `MOD_*` flags fire on either side — the caller must use the
    // generic name for the RegisterHotKey driver.
    let parsed = parse_chord(&s(&["ctrl", "f9"])).expect("ctrl+f9 parses");
    assert_eq!(parsed.mods, MOD_CONTROL);
    assert_eq!(parsed.vk, 0x78);
}

#[test]
fn parses_all_four_modifier_families_with_trigger() {
    // Every MOD_* flag must OR into the mask; the trigger stays a
    // single VK. The permutation catches an accidental single-family
    // reset in `parse_chord` (each mod bit is independent).
    let parsed = parse_chord(&s(&["ctrl", "shift", "alt", "cmd", "f10"])).expect("parses");
    assert_eq!(parsed.mods, MOD_CONTROL | MOD_SHIFT | MOD_ALT | MOD_WIN);
    assert_eq!(parsed.vk, 0x79);
}

#[test]
fn generic_alt_maps_to_mod_alt() {
    // Side-specific alt aliases are rejected up-front (see
    // `rejects_side_specific_modifier_aliases`); the generic `alt`
    // still maps to MOD_ALT so users who don't care which side is
    // pressed get a valid install.
    let parsed = parse_chord(&s(&["alt", "f9"])).expect("alt+f9 parses");
    assert_eq!(parsed.mods, MOD_ALT);
}

#[test]
fn generic_win_and_cmd_names_map_to_mod_win() {
    // The tracker names the Windows key as `cmd` (macOS vocabulary —
    // inherited from pynput); the RegisterHotKey flag is MOD_WIN. Both
    // generic `cmd` and the friendlier generic `win` names are accepted;
    // the sided variants (`cmd_l`, `win_r`, …) are rejected up-front
    // because the OS flag fires on either side.
    for win_name in ["cmd", "win"] {
        let parsed = parse_chord(&s(&[win_name, "f9"]))
            .unwrap_or_else(|e| panic!("win alias {win_name:?} must parse: {e}"));
        assert_eq!(parsed.mods, MOD_WIN);
    }
}

#[test]
fn rejects_side_specific_modifier_aliases() {
    // `MOD_CONTROL` fires on
    // EITHER Ctrl side, so registering `ctrl_r+f9` would also match
    // `ctrl_l+f9` — the opposite of what the user configured. The
    // driver rejects side-specific aliases at parse time so the
    // supervisor falls back to rdev (which tracks sides accurately).
    //
    // The full list mirrors `is_side_specific_modifier`: two per family
    // (Ctrl / Shift / Alt / Win / Cmd) plus the right-Alt aliases the
    // rdev driver accepts (`alt_gr`, `right_alt`, `ralt`) since those
    // all name the right-Alt key specifically.
    for sided in [
        "ctrl_l",
        "ctrl_r",
        "shift_l",
        "shift_r",
        "alt_l",
        "alt_r",
        "alt_gr",
        "right_alt",
        "ralt",
        "cmd_l",
        "cmd_r",
        "win_l",
        "win_r",
    ] {
        let err = parse_chord(&s(&[sided, "f9"]))
            .expect_err(&format!("side-specific alias {sided:?} must be rejected"));
        assert!(
            err.contains("side-specific") || err.contains(sided),
            "message must name the constraint or the offending alias: {err} (alias={sided:?})"
        );
        assert!(
            err.contains("rdev") || err.contains("generic"),
            "message must point at the escape hatch (rdev fallback / generic name): {err}"
        );
    }
}

#[test]
fn is_side_specific_modifier_matches_the_reject_set() {
    // The rejection list must stay in sync with the helper used to test
    // it (parse_chord itself uses the helper), so cover the helper
    // directly here as well.
    for sided in [
        "ctrl_l",
        "ctrl_r",
        "shift_l",
        "shift_r",
        "alt_l",
        "alt_r",
        "alt_gr",
        "right_alt",
        "ralt",
        "cmd_l",
        "cmd_r",
        "win_l",
        "win_r",
    ] {
        assert!(
            is_side_specific_modifier(sided),
            "{sided:?} should be side-specific"
        );
    }
    for generic in ["ctrl", "shift", "alt", "cmd", "win"] {
        assert!(
            !is_side_specific_modifier(generic),
            "generic {generic:?} must NOT be side-specific"
        );
    }
    // Trigger names should never register as side-specific modifiers.
    for trig in ["f9", "space", "esc", "a", "1"] {
        assert!(!is_side_specific_modifier(trig));
    }
}

#[test]
fn rejects_modifier_only_chord_with_actionable_message() {
    // The signature limitation of RegisterHotKey: modifier-only chords
    // are NOT supported. The error message must name the constraint
    // AND point the user at the escape hatch (VOICEPI_HOTKEY_DRIVER=rdev)
    // so a user seeing this in the diagnostic log can fix it without
    // reading source.
    let err = parse_chord(&s(&["ctrl"])).expect_err("bare modifier must be rejected");
    assert!(
        err.contains("only modifiers") || err.contains("modifier"),
        "message must call out the modifier-only constraint: {err}"
    );
    assert!(
        err.contains("rdev") || err.contains("trigger"),
        "message must point at the escape hatch: {err}"
    );
}

#[test]
fn rejects_multiple_modifier_only_binding() {
    // Multiple modifiers with no trigger: same limitation, same
    // rejection. Guards against a future refactor that would treat
    // "N modifiers, N > 1" as "chord present".
    let err = parse_chord(&s(&["ctrl", "shift"])).expect_err("bare modifiers must be rejected");
    assert!(err.contains("modifier"), "{err}");
}

#[test]
fn rejects_two_non_modifier_triggers() {
    // RegisterHotKey binds ONE trigger VK per hotkey id. A chord with
    // two triggers (e.g. `f9+f10`) is a config error we surface up-
    // front rather than silently registering only the first.
    let err = parse_chord(&s(&["f9", "f10"])).expect_err("two triggers must be rejected");
    assert!(err.contains("more than one"), "{err}");
    // Both names must appear so the user can see which two collided.
    assert!(err.contains("f9") && err.contains("f10"), "{err}");
}

#[test]
fn rejects_unsupported_trigger_name() {
    // Anything outside f1..f12, ASCII letter/digit, space/esc/tab/enter/pause
    // is rejected with a message that names the offender. Guards against
    // silent no-op installs when a user copy-pastes an rdev-only name.
    let err = parse_chord(&s(&["insert"])).expect_err("insert has no VK mapping");
    assert!(err.contains("insert"), "{err}");
    assert!(err.contains("supported"), "{err}");
}

#[test]
fn accepts_letter_and_digit_triggers_via_ascii_passthrough() {
    // Letters and digits map to their ASCII byte value — the Windows
    // VK table for A..Z and 0..9 is literally the ASCII byte, so a
    // one-character segment is a valid trigger. This is the only path
    // through `vk_from_trigger_name` that returns without a lookup
    // table entry, so it gets its own test. Uses generic `ctrl` /
    // `shift` names because side-specific modifiers are rejected up-
    // front now.
    let a = parse_chord(&s(&["a"])).expect("letter a parses");
    assert_eq!(a.vk, 0x41);
    let z = parse_chord(&s(&["ctrl", "z"])).expect("ctrl+z parses");
    assert_eq!(z.mods, MOD_CONTROL);
    assert_eq!(z.vk, 0x5A);
    let one = parse_chord(&s(&["shift", "1"])).expect("shift+1 parses");
    assert_eq!(one.mods, MOD_SHIFT);
    assert_eq!(one.vk, 0x31);
}

#[test]
fn empty_chord_and_whitespace_segments_report_error() {
    // Empty input is explicitly rejected (upstream `install_hotkey`
    // catches this via `EmptyConfig`, but the driver's parser must
    // agree so a direct caller gets a clear message too).
    let err = parse_chord(&[]).expect_err("empty chord rejected");
    assert!(err.contains("empty"), "{err}");
    // Whitespace-only segments (a user typing `ctrl+  +f9`) are
    // dropped so the config accepts benign spacing without producing a
    // silent parse failure downstream.
    let parsed = parse_chord(&s(&["ctrl", "   ", "f9"])).expect("padded chord parses");
    assert_eq!(parsed.mods, MOD_CONTROL);
    assert_eq!(parsed.vk, 0x78);
}

#[test]
fn parse_is_case_insensitive_and_trims() {
    // The tracker's names are lowercase; a config with `CTRL+F9`
    // should install just fine. Trim + ASCII-lowercase before matching.
    let parsed = parse_chord(&s(&["  CTRL  ", "F9"])).expect("case-insensitive parse");
    assert_eq!(parsed.mods, MOD_CONTROL);
    assert_eq!(parsed.vk, 0x78);
}

#[test]
fn side_specific_rejection_is_case_insensitive() {
    // A user typing `CTRL_L+F9` should hit the same side-specific
    // rejection as the lowercase form — the guard runs after trim +
    // lowercase, so case must not smuggle a sided alias through.
    let err = parse_chord(&s(&["CTRL_L", "F9"])).expect_err("upper-case CTRL_L is still sided");
    assert!(
        err.contains("side-specific") || err.contains("ctrl_l"),
        "{err}"
    );
}

#[test]
fn vk_helper_returns_none_for_names_the_parser_would_reject() {
    // Belt-and-braces: `vk_from_trigger_name` is exposed pub(crate) for
    // the parser + one telemetry site. Anything the parser rejects
    // must also return None here so the two decisions cannot drift.
    assert!(vk_from_trigger_name("insert").is_none());
    assert!(vk_from_trigger_name("ab").is_none()); // multi-char non-lookup
    assert!(vk_from_trigger_name("!").is_none()); // punctuation
}

#[test]
fn display_string_preserves_input_order_after_trim_and_lowercase() {
    // The `display` field is what the diagnostic log line names —
    // preserving segment order (not `mods|vk` reordering) helps
    // operators grep for exactly the chord they typed.
    let parsed = parse_chord(&s(&["Shift", "CTRL", "f9"])).expect("parses");
    assert_eq!(parsed.display, "shift+ctrl+f9");
}

#[test]
fn pause_key_is_a_supported_trigger() {
    // `pause` maps to the Windows virtual key VK_PAUSE (0x13).
    let parsed = parse_chord(&s(&["pause"])).expect("pause parses");
    assert_eq!(parsed.vk, 0x13);
    assert_eq!(parsed.mods, 0);
}
