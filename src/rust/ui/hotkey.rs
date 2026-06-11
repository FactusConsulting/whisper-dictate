//! Pure validation of the push-to-talk / quit `key` chord string.
//!
//! The hotkey field (`settings.key`, e.g. `shift_l+ctrl_l`) is a `+`-joined chord
//! of key *tokens*. The Python keyboard layer resolves each token to a real key
//! before the listener can run; an unknown token makes the worker `sys.exit` at
//! startup with `unknown key '<tok>'`. To give the user that feedback inside the
//! settings UI — before they ever start the worker — this module reproduces the
//! Python acceptance rules as a pure, unit-tested function.
//!
//! ## Where the canonical token set comes from
//!
//! The matcher is `_pynput_targets` in `src/python/whisper_dictate/vp_keys.py`
//! (and the evdev twin `_evdev_target_codes`). For the pynput backend each token
//! `kn` must resolve via `getattr(keyboard.Key, kn)` — i.e. **a token is valid
//! iff it is the name of a `pynput.keyboard.Key` enum member**. (Single
//! characters are NOT accepted for the PTT key, unlike the quit key.) The
//! authoritative member list is the cross-platform base enum in pynput's
//! `keyboard/_base.py` (`Key.alt … Key.scroll_lock`, plus `f1..f20`); per-platform
//! subclasses only map some of those to vk 0, they never add names the base lacks.
//!
//! We therefore accept exactly that base-enum name set. This is the SUPERSET
//! across X11 / Windows / macOS, which is correct: a config travels between
//! platforms, so the validator must never reject a name that pynput would accept
//! on *some* supported OS.
//!
//! ## Known divergence from the evdev backend (intentional)
//!
//! The Wayland evdev backend (`_EVDEV_MAP`) resolves a *narrower* set —
//! `ctrl_l/r`, `shift_l/r`, `alt_l/r`, `super_l/r`, `f1..f12`. Tokens this module
//! marks valid that evdev cannot map (e.g. `space`, `f13`, `cmd`, `caps_lock`)
//! will `sys.exit` only on pure-Wayland-with-evdev. We follow the pynput set
//! (the broad, default backend) deliberately; flagging the evdev subset in the UI
//! would wrongly reject keys that work everywhere else.

/// Outcome of validating a hotkey chord string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum HotkeyValidation {
    /// The chord is well-formed and every token is an accepted key name.
    Valid,
    /// The chord is rejected; carries the specific reason class.
    Invalid(HotkeyError),
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

/// The canonical accepted key-name tokens — the names of the `pynput.keyboard.Key`
/// enum members from pynput's cross-platform `keyboard/_base.py`. F-keys are
/// handled by [`is_function_key`] rather than listed individually.
///
/// Kept sorted for readability; lookup is a linear scan over this small set.
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
    "shift",
    "shift_l",
    "shift_r",
    "super_l",
    "super_r", // evdev super names, harmless for pynput too
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
    // media / consumer-control (resolvable Key members; the solo guard ignores them
    // at runtime, but pynput still resolves the *name*, so they are valid tokens)
    "media_play_pause",
    "media_volume_mute",
    "media_volume_down",
    "media_volume_up",
    "media_previous",
    "media_next",
    "media_stop",
];

/// True for a function-key token `f1`..`f24`. pynput's base enum defines `f1..f20`
/// and some macOS builds extend to `f24`; accepting through `f24` never wrongly
/// rejects a key pynput would resolve on a supported platform.
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

/// Validate a chord string exactly as the Python pynput layer would accept it.
///
/// Splits on `+` and trims each token (matching `self.key.split('+')` +
/// `n.strip()` in `vp_keys.run`). Returns the first failure encountered, in this
/// order: empty whole string, a blank token, an unknown token, a duplicate token.
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

/// The accepted modifier tokens, for the UI reference line. A curated, ordered
/// subset of [`KEY_NAMES`] (the keys users actually bind to PTT) — not every
/// resolvable name, so the reference stays short and useful.
pub(in crate::ui) const REFERENCE_MODIFIERS: &str =
    "ctrl_l, ctrl_r, shift_l, shift_r, alt_l, alt_r, alt_gr, cmd, super_l, super_r";

/// Example named keys for the UI reference line (function keys + a few common
/// editing/navigation keys). The full set is large; this shows the shape.
pub(in crate::ui) const REFERENCE_KEYS: &str =
    "f1–f24, esc, tab, space, enter, insert, delete, home, end, page_up, page_down";

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
    fn valid_named_keys() {
        for token in [
            "esc",
            "tab",
            "space",
            "enter",
            "insert",
            "home",
            "media_play_pause",
        ] {
            assert!(validate_hotkey(token).is_valid(), "{token} should be valid");
        }
    }

    #[test]
    fn valid_chord_of_modifier_and_function_key() {
        // ctrl+f9 is a mixed binding (not solo-guarded in Python, but valid).
        assert!(validate_hotkey("ctrl_l+f9").is_valid());
    }

    #[test]
    fn valid_with_surrounding_and_inner_whitespace() {
        // run() trims each split token, so spaces around tokens are accepted.
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
        // A bare single letter is NOT a valid PTT target (pynput Key has no `a`).
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
            if token == "f1–f24" {
                continue; // a range label, not a single token
            }
            assert!(is_valid_key_token(token), "ref key {token} invalid");
        }
    }
}
