//! Validate hotkeys and provide the small state machine used by shortcut capture.
//!
//! A chord is syntactically valid when it contains known key names separated by
//! `+`. Native support is narrower: the common listeners handle the same small
//! set of modifiers and triggers, while Windows `RegisterHotKey` has additional
//! limits on side-specific, modifier-only, and multi-trigger chords. Keeping the
//! two checks separate lets the settings UI accept existing config names while
//! warning before a binding is saved that may not work.

/// Outcome of validating a hotkey chord string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum HotkeyValidation {
    /// The chord is well-formed and every token is an accepted key name.
    Valid,
    /// The chord is rejected; carries the specific reason class.
    Invalid(HotkeyError),
}

/// A syntactically valid chord that is not reliable on every native listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum HotkeyWarning {
    /// The token is known to the configuration format but is not in the common
    /// native key set.
    UnsupportedToken(String),
    /// Windows must use the low-level fallback for this chord shape.
    WindowsFallback,
}

/// Why a chord string is invalid. Each variant maps to a localized message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum HotkeyError {
    /// The string is empty or only whitespace / bare `+` separators.
    Empty,
    /// A `+`-separated segment is blank (e.g. `ctrl_l+`, `a++b`, leading `+`).
    EmptyToken,
    /// A token is not an accepted key name. Carries the offending token.
    UnknownToken(String),
    /// The same token appears more than once. Carries the duplicated token.
    DuplicateToken(String),
}

impl HotkeyValidation {
    /// True when the chord parsed cleanly. Used by the tests and available to any
    /// caller that only needs the boolean; the UI matches on the full result.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::ui) fn is_valid(&self) -> bool {
        matches!(self, HotkeyValidation::Valid)
    }
}

/// Known configuration tokens. Some are retained for compatibility but produce
/// a capability warning because they are not in the common native set below.
const KEY_NAMES: &[&str] = &[
    // modifiers (the common PTT keys)
    "alt",
    "alt_l",
    "alt_r",
    "alt_gr",
    "cmd",
    "cmd_l",
    "cmd_r",
    "ctrl",
    "ctrl_l",
    "ctrl_r",
    "win",
    "win_l",
    "win_r",
    "right_alt",
    "ralt",
    "shift",
    "shift_l",
    "shift_r",
    "super_l",
    "super_r",
    // editing / navigation / whitespace
    "backspace",
    "delete",
    "enter",
    "esc",
    "space",
    "tab",
    "insert",
    "home",
    "end",
    "page_up",
    "page_down",
    "up",
    "down",
    "left",
    "right", // locks / system
    "caps_lock",
    "num_lock",
    "scroll_lock",
    "pause",
    "print_screen",
    "menu",
    // media / consumer-control names retained by the settings format
    "media_play_pause",
    "media_volume_mute",
    "media_volume_down",
    "media_volume_up",
    "media_previous",
    "media_next",
    "media_stop",
];

/// True for a function-key token `f1`..`f24`.
fn is_function_key(token: &str) -> bool {
    let Some(num) = token.strip_prefix('f') else {
        return false;
    };
    // Reject `f`, `f0`, `f007`, `f3a` — must be a plain 1..=24 with no leading zero.
    if num.is_empty() || (num.len() > 1 && num.starts_with('0')) {
        return false;
    }
    matches!(num.parse::<u32>(), Ok(n) if (1..=24).contains(&n))
}

/// True if `token` is an accepted hotkey key name. Pure; the single source of
/// truth for both the validator and the reference shown in the UI.
pub(in crate::ui) fn is_valid_key_token(token: &str) -> bool {
    KEY_NAMES.contains(&token) || is_function_key(token)
}

const COMMON_MODIFIERS: &[&str] = &[
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
];

const COMMON_TRIGGERS: &[&str] = &["pause", "space", "esc", "tab", "enter"];

fn is_modifier_token(token: &str) -> bool {
    COMMON_MODIFIERS.contains(&token)
}

fn is_side_specific_modifier(token: &str) -> bool {
    matches!(
        token,
        "ctrl_l"
            | "ctrl_r"
            | "shift_l"
            | "shift_r"
            | "alt_l"
            | "alt_r"
            | "alt_gr"
            | "right_alt"
            | "ralt"
            | "cmd_l"
            | "cmd_r"
            | "win_l"
            | "win_r"
    )
}

fn is_common_native_token(token: &str) -> bool {
    COMMON_MODIFIERS.contains(&token)
        || COMMON_TRIGGERS.contains(&token)
        || matches!(
            token.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()),
            Some(1..=12)
        )
}

/// Validate a chord string. Splits on `+`, trims each token, and reports the
/// first failure: empty input, a blank token, an unknown token, or a duplicate.
pub(in crate::ui) fn validate_hotkey(chord: &str) -> HotkeyValidation {
    let trimmed = chord.trim();
    if trimmed.is_empty() {
        return HotkeyValidation::Invalid(HotkeyError::Empty);
    }
    let tokens: Vec<&str> = trimmed.split('+').map(str::trim).collect();
    // A string that is only separators/whitespace (e.g. "+", " + ") yields all
    // empty tokens — surface that as an empty-token error, not "empty".
    let mut seen: Vec<&str> = Vec::with_capacity(tokens.len());
    for token in &tokens {
        if token.is_empty() {
            return HotkeyValidation::Invalid(HotkeyError::EmptyToken);
        }
        if !is_valid_key_token(token) {
            return HotkeyValidation::Invalid(HotkeyError::UnknownToken((*token).to_owned()));
        }
        if seen.contains(token) {
            return HotkeyValidation::Invalid(HotkeyError::DuplicateToken((*token).to_owned()));
        }
        seen.push(token);
    }
    HotkeyValidation::Valid
}

fn hotkey_warning_for_platform(chord: &str, windows: bool) -> Option<HotkeyWarning> {
    if !validate_hotkey(chord).is_valid() {
        return None;
    }
    let tokens: Vec<&str> = chord.trim().split('+').map(str::trim).collect();
    if let Some(token) = tokens.iter().find(|token| !is_common_native_token(token)) {
        return Some(HotkeyWarning::UnsupportedToken((*token).to_owned()));
    }
    if windows {
        let trigger_count = tokens
            .iter()
            .filter(|token| !is_modifier_token(token))
            .count();
        if trigger_count != 1 || tokens.iter().any(|token| is_side_specific_modifier(token)) {
            return Some(HotkeyWarning::WindowsFallback);
        }
    }
    None
}

/// Return a capability warning for a valid chord on the current desktop.
pub(in crate::ui) fn hotkey_warning(chord: &str) -> Option<HotkeyWarning> {
    hotkey_warning_for_platform(chord, cfg!(target_os = "windows"))
}

/// Names that work across the native listeners in the normal path.
pub(in crate::ui) const REFERENCE_MODIFIERS: &str =
    "ctrl, shift, alt, cmd, win, ctrl_l, ctrl_r, shift_l, shift_r, alt_l, alt_r, alt_gr, right_alt, ralt, cmd_l, cmd_r, win_l, win_r";

pub(in crate::ui) const REFERENCE_KEYS: &str = "pause, f1–f12, esc, tab, space, enter";

#[cfg(test)]
mod tests {
    use super::*;

    fn err(chord: &str) -> HotkeyError {
        match validate_hotkey(chord) {
            HotkeyValidation::Invalid(e) => e,
            HotkeyValidation::Valid => panic!("expected invalid for {chord:?}"),
        }
    }

    #[test]
    fn valid_single_modifier() {
        assert!(validate_hotkey("ctrl_r").is_valid());
        assert!(validate_hotkey("shift_l").is_valid());
        assert!(validate_hotkey("alt_gr").is_valid());
        assert!(validate_hotkey("cmd").is_valid());
    }

    #[test]
    fn valid_modifier_chord() {
        assert!(validate_hotkey("shift_l+ctrl_l").is_valid());
        assert!(validate_hotkey("alt_l+shift_l+ctrl_l").is_valid());
    }

    #[test]
    fn valid_function_keys_f1_through_f24() {
        for n in 1..=24 {
            let token = format!("f{n}");
            assert!(
                validate_hotkey(&token).is_valid(),
                "{token} should be valid"
            );
        }
    }

    #[test]
    fn valid_named_tokens() {
        for token in [
            "esc",
            "tab",
            "space",
            "enter",
            "pause",
            "insert",
            "home",
            "media_play_pause",
        ] {
            assert!(validate_hotkey(token).is_valid(), "{token} should be valid");
        }
    }

    #[test]
    fn valid_chord_of_modifier_and_function_key() {
        // ctrl+f9 is a valid modifier-plus-trigger chord.
        assert!(validate_hotkey("ctrl_l+f9").is_valid());
    }

    #[test]
    fn valid_with_surrounding_and_inner_whitespace() {
        // Whitespace around tokens is ignored.
        assert!(validate_hotkey("  shift_l + ctrl_l  ").is_valid());
    }

    #[test]
    fn invalid_empty_string() {
        assert_eq!(err(""), HotkeyError::Empty);
        assert_eq!(err("   "), HotkeyError::Empty);
    }

    #[test]
    fn invalid_blank_token_from_stray_plus() {
        assert_eq!(err("ctrl_l+"), HotkeyError::EmptyToken);
        assert_eq!(err("+ctrl_l"), HotkeyError::EmptyToken);
        assert_eq!(err("ctrl_l++shift_l"), HotkeyError::EmptyToken);
        // A string of only separators is all-empty tokens, not Empty.
        assert_eq!(err("+"), HotkeyError::EmptyToken);
        assert_eq!(err(" + "), HotkeyError::EmptyToken);
    }

    #[test]
    fn invalid_unknown_token() {
        assert_eq!(err("foo"), HotkeyError::UnknownToken("foo".to_owned()));
        // Single letters are not key names in the cross-platform setting format.
        assert_eq!(err("a"), HotkeyError::UnknownToken("a".to_owned()));
        // Unknown wins over a later duplicate.
        assert_eq!(
            err("ctrl_l+bogus+ctrl_l"),
            HotkeyError::UnknownToken("bogus".to_owned())
        );
    }

    #[test]
    fn invalid_function_key_out_of_range_or_malformed() {
        assert_eq!(err("f0"), HotkeyError::UnknownToken("f0".to_owned()));
        assert_eq!(err("f25"), HotkeyError::UnknownToken("f25".to_owned()));
        assert_eq!(err("f"), HotkeyError::UnknownToken("f".to_owned()));
        assert_eq!(err("f01"), HotkeyError::UnknownToken("f01".to_owned()));
        assert_eq!(err("f3a"), HotkeyError::UnknownToken("f3a".to_owned()));
    }

    #[test]
    fn invalid_duplicate_token() {
        assert_eq!(
            err("ctrl_l+ctrl_l"),
            HotkeyError::DuplicateToken("ctrl_l".to_owned())
        );
        // Duplicate detection runs after trimming, so spacing doesn't hide it.
        assert_eq!(
            err("ctrl_l + shift_l + ctrl_l"),
            HotkeyError::DuplicateToken("ctrl_l".to_owned())
        );
    }

    #[test]
    fn warns_for_names_outside_the_common_native_set() {
        assert_eq!(
            hotkey_warning_for_platform("insert", false),
            Some(HotkeyWarning::UnsupportedToken("insert".to_owned()))
        );
        assert_eq!(
            hotkey_warning_for_platform("ctrl+f13", false),
            Some(HotkeyWarning::UnsupportedToken("f13".to_owned()))
        );
        assert_eq!(hotkey_warning_for_platform("ctrl+f9", false), None);
    }

    #[test]
    fn warns_when_windows_needs_the_fallback_listener() {
        assert_eq!(
            hotkey_warning_for_platform("ctrl_l+f9", true),
            Some(HotkeyWarning::WindowsFallback)
        );
        assert_eq!(
            hotkey_warning_for_platform("ctrl", true),
            Some(HotkeyWarning::WindowsFallback)
        );
        assert_eq!(hotkey_warning_for_platform("pause", true), None);
        assert_eq!(hotkey_warning_for_platform("ctrl+pause", true), None);
        assert_eq!(hotkey_warning_for_platform("ctrl+f9", true), None);
    }

    #[test]
    fn is_function_key_boundaries() {
        assert!(is_function_key("f1"));
        assert!(is_function_key("f24"));
        assert!(!is_function_key("f0"));
        assert!(!is_function_key("f25"));
        assert!(!is_function_key("f"));
        assert!(!is_function_key("f01"));
        assert!(!is_function_key("ctrl_l"));
    }

    #[test]
    fn every_reference_token_is_actually_valid() {
        // Guard: the strings shown to the user must all pass validation, so the
        // reference can never advertise a token the validator rejects.
        for token in REFERENCE_MODIFIERS.split(", ") {
            assert!(is_valid_key_token(token), "ref modifier {token} invalid");
        }
        for token in REFERENCE_KEYS.split(", ") {
            if token == "f1–f12" {
                continue; // a range label, not a single token
            }
            assert!(is_valid_key_token(token), "ref key {token} invalid");
        }
    }
}
