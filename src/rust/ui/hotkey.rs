//! Validate hotkeys and provide the small state machine used by shortcut capture.
//!
//! A chord is syntactically valid when it contains known key names separated by
//! `+`. Native support is narrower: the common listeners handle the same small
//! set of modifiers and triggers, while Windows `RegisterHotKey` has additional
//! limits on side-specific, modifier-only, and multi-trigger chords. Keeping the
//! two checks separate lets the settings UI accept existing config names while
//! showing whether the current session can install it and which concrete
//! listener would be selected.

/// Outcome of validating a hotkey chord string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum HotkeyValidation {
    /// The chord is well-formed and every token is an accepted key name.
    Valid,
    /// The chord is rejected; carries the specific reason class.
    Invalid(HotkeyError),
}

/// Driver-aware capability classification for the current desktop session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum HotkeyCapability {
    Invalid(HotkeyError),
    Unsupported(String),
    FallbackRisk {
        planned_driver: String,
        reason: Option<String>,
    },
    Installable {
        planned_driver: String,
    },
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

/// Canonical spelling used for identity comparisons and subprocess arguments.
/// Validation remains separate so this helper is also safe for stale/invalid
/// session values: it only trims tokens and joins them with one separator.
pub(in crate::ui) fn canonical_hotkey(chord: &str) -> String {
    chord
        .trim()
        .split('+')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("+")
}

/// The secondary actions are owned by RegisterHotKey, which accepts a narrower
/// chord grammar than the main PTT listener can on its rdev fallback.
#[cfg(windows)]
fn validate_action_hotkey(value: &str, ptt: &str, action_label: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Ok(());
    }
    #[cfg(feature = "rust-hotkeys")]
    {
        use crate::hotkey::manager::win_registerhotkey::{parse_chord, same_chord};
        let names = |raw: &str| {
            raw.split('+')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let action = parse_chord(&names(value))?;
        if action.trigger_name == "f12" {
            return Err(
                "F12 is reserved by Windows and cannot be registered as a shortcut".to_owned(),
            );
        }
        let primary = parse_chord(&names(ptt)).map_err(|error| {
            format!("{action_label} needs a PTT chord supported by Windows RegisterHotKey: {error}")
        })?;
        if same_chord(&action, &primary) {
            return Err(format!("PTT and {action_label} shortcuts must differ"));
        }
        Ok(())
    }
    #[cfg(not(feature = "rust-hotkeys"))]
    {
        let _ = (ptt, action_label);
        Err(format!(
            "{action_label} shortcut requires a build with rust-hotkeys"
        ))
    }
}

/// Validate the copy-last shortcut against the PTT chord and, when
/// paste-last is enabled, reject ctrl+v: the paste arm injects that
/// exact chord and would re-trigger copy-last on every burst (Codex P2
/// win_registerhotkey.rs:572). A blank paste-last binding keeps ctrl+v
/// available for copy-last.
#[cfg(windows)]
#[cfg_attr(not(feature = "rust-hotkeys"), allow(unused_variables))]
pub(in crate::ui) fn validate_copy_last_hotkey(
    value: &str,
    ptt: &str,
    paste_last: &str,
) -> Result<(), String> {
    validate_action_hotkey(value, ptt, "copy last")?;
    #[cfg(feature = "rust-hotkeys")]
    {
        use crate::hotkey::manager::win_registerhotkey::{parse_chord, same_chord};
        let names = |raw: &str| {
            raw.split('+')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        if !paste_last.trim().is_empty() {
            if let (Ok(copy), Ok(ctrl_v)) = (
                parse_chord(&names(value)),
                parse_chord(&["ctrl".to_owned(), "v".to_owned()]),
            ) {
                if same_chord(&copy, &ctrl_v) {
                    return Err(
                        "copy-last cannot use ctrl+v while paste-last is enabled: the injected paste chord would re-trigger copy-last"
                            .to_owned(),
                    );
                }
            }
        }
    }
    Ok(())
}

/// Validate the paste-last shortcut against the PTT chord AND the copy-last
/// chord (either may be blank, which skips that check).
#[cfg(windows)]
pub(in crate::ui) fn validate_paste_last_hotkey(
    value: &str,
    ptt: &str,
    copy_last: &str,
) -> Result<(), String> {
    validate_action_hotkey(value, ptt, "paste last")?;
    #[cfg(feature = "rust-hotkeys")]
    {
        use crate::hotkey::manager::win_registerhotkey::{parse_chord, same_chord};
        let names = |raw: &str| {
            raw.split('+')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        if let Ok(paste) = parse_chord(&names(value)) {
            // The paste arm injects the plain Ctrl+V chord itself and
            // RegisterHotKey posts WM_HOTKEY for synthetic key events, so
            // binding paste-last to exactly ctrl+v re-triggers its own
            // paste the moment the burst starts (the worker clears its
            // busy flag before the message loop drains the synthesized
            // hotkey). Reject it at entry so the user sees the reason
            // while configuring (Codex P1 ui/hotkey.rs:224); the
            // RegisterHotKey driver rejects it too for hand-edited
            // config.json.
            if let Ok(ctrl_v) = parse_chord(&["ctrl".to_owned(), "v".to_owned()]) {
                if same_chord(&paste, &ctrl_v) {
                    return Err(
                        "paste-last cannot use ctrl+v: the injected paste chord would re-trigger the shortcut"
                            .to_owned(),
                    );
                }
            }
            // Symmetric conflict (Codex P2 win_registerhotkey.rs:572):
            // when copy-last already owns ctrl+v, enabling paste-last
            // would make every paste burst re-trigger copy-last. The
            // paste arm refuses the same combination at registration.
            if !copy_last.trim().is_empty() {
                if let (Ok(copy), Ok(ctrl_v)) = (
                    parse_chord(&names(copy_last)),
                    parse_chord(&["ctrl".to_owned(), "v".to_owned()]),
                ) {
                    if same_chord(&copy, &ctrl_v) {
                        return Err(
                            "paste-last cannot be enabled while copy-last uses ctrl+v: the injected paste chord would re-trigger copy-last"
                                .to_owned(),
                        );
                    }
                }
            }
            // Symmetric conflict (Codex P2 win_registerhotkey.rs:620):
            // when PTT already owns ctrl+v, enabling paste-last would
            // start an unintended recording on every paste burst. The
            // PTT arm refuses the same combination at registration.
            if !ptt.trim().is_empty() {
                if let (Ok(ptt_chord), Ok(ctrl_v)) = (
                    parse_chord(&names(ptt)),
                    parse_chord(&["ctrl".to_owned(), "v".to_owned()]),
                ) {
                    if same_chord(&ptt_chord, &ctrl_v) {
                        return Err(
                            "paste-last cannot be enabled while PTT uses ctrl+v: the injected paste chord would re-trigger PTT"
                                .to_owned(),
                        );
                    }
                }
            }
        }
    }
    if value.trim().is_empty() || copy_last.trim().is_empty() {
        return Ok(());
    }
    #[cfg(feature = "rust-hotkeys")]
    {
        use crate::hotkey::manager::win_registerhotkey::{parse_chord, same_chord};
        let names = |raw: &str| {
            raw.split('+')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        if let (Ok(paste), Ok(copy)) = (parse_chord(&names(value)), parse_chord(&names(copy_last)))
        {
            if same_chord(&paste, &copy) {
                return Err("copy-last and paste-last shortcuts must differ".to_owned());
            }
        }
        Ok(())
    }
    #[cfg(not(feature = "rust-hotkeys"))]
    {
        let _ = copy_last;
        Err("paste last shortcut requires a build with rust-hotkeys".to_owned())
    }
}

/// Validate a mode shortcut (cycle/raw/clean) against the PTT chord.
/// Mode shortcuts never inject, so unlike paste-last there is no
/// paste/copy coordination — only the ctrl+v refusal, which mirrors the
/// driver's registration guard: RegisterHotKey intercepts the chord
/// globally, so a ctrl+v mode binding would swallow normal pasting.
#[cfg(windows)]
pub(in crate::ui) fn validate_mode_hotkey(
    value: &str,
    ptt: &str,
    action_label: &str,
) -> Result<(), String> {
    validate_action_hotkey(value, ptt, action_label)?;
    #[cfg(feature = "rust-hotkeys")]
    {
        use crate::hotkey::manager::win_registerhotkey::{parse_chord, same_chord};
        let names = |raw: &str| {
            raw.split('+')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        if let (Ok(chord), Ok(ctrl_v)) = (
            parse_chord(&names(value)),
            parse_chord(&["ctrl".to_owned(), "v".to_owned()]),
        ) {
            if same_chord(&chord, &ctrl_v) {
                return Err(format!(
                    "{action_label} cannot use ctrl+v: the binding would intercept normal pasting"
                ));
            }
        }
        Ok(())
    }
    #[cfg(not(feature = "rust-hotkeys"))]
    {
        let _ = (value, ptt);
        Err(format!(
            "{action_label} shortcut requires a build with rust-hotkeys"
        ))
    }
}

/// Validate all three mode shortcuts together against every configured
/// action chord before a save. The RegisterHotKey driver already rejects
/// collisions at registration, but the first registered owner wins there
/// and the later binding silently dies with a stderr warning — Settings
/// must refuse the combination up front so a configured shortcut is never
/// left inactive (Codex P2 ui/hotkey.rs:359).
#[cfg(windows)]
#[cfg_attr(not(feature = "rust-hotkeys"), allow(unused_variables))]
pub(in crate::ui) fn validate_mode_shortcuts(
    cycle: &str,
    raw: &str,
    clean: &str,
    ptt: &str,
    copy_last: &str,
    paste_last: &str,
) -> Result<(), String> {
    for (value, label) in [
        (cycle, "cycle mode"),
        (raw, "raw mode"),
        (clean, "clean mode"),
    ] {
        validate_mode_hotkey(value, ptt, label)?;
    }
    #[cfg(feature = "rust-hotkeys")]
    {
        use crate::hotkey::manager::win_registerhotkey::{parse_chord, same_chord};
        let names = |raw_input: &str| {
            raw_input
                .split('+')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let modes = [
            ("cycle mode", cycle),
            ("raw mode", raw),
            ("clean mode", clean),
        ];
        for (label, value) in modes {
            if let Ok(chord) = parse_chord(&names(value)) {
                for (other_label, other) in [("copy-last", copy_last), ("paste-last", paste_last)] {
                    if let Ok(other_chord) = parse_chord(&names(other)) {
                        if same_chord(&chord, &other_chord) {
                            return Err(format!("{other_label} and {label} shortcuts must differ"));
                        }
                    }
                }
            }
        }
        // The three mode shortcuts must also differ from each other.
        for (index, (label, value)) in modes.iter().enumerate() {
            if let Ok(chord) = parse_chord(&names(value)) {
                for (other_label, other) in modes.iter().skip(index + 1) {
                    if let Ok(other_chord) = parse_chord(&names(other)) {
                        if same_chord(&chord, &other_chord) {
                            return Err(format!("{other_label} and {label} shortcuts must differ"));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Classify syntax and the concrete listener plan without installing anything.
pub(in crate::ui) fn hotkey_capability(chord: &str) -> HotkeyCapability {
    if let HotkeyValidation::Invalid(err) = validate_hotkey(chord) {
        return HotkeyCapability::Invalid(err);
    }
    let key_names = chord
        .split('+')
        .map(str::trim)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    capability_from_preflight(crate::hotkey::preflight_hotkey(&key_names))
}

pub(in crate::ui) fn capability_from_preflight(
    preflight: crate::hotkey::Result<crate::hotkey::HotkeyPreflight>,
) -> HotkeyCapability {
    match preflight {
        Err(err) => HotkeyCapability::Unsupported(err.to_string()),
        Ok(preflight) if preflight.focused_window_risk => HotkeyCapability::FallbackRisk {
            planned_driver: preflight.planned_driver.to_owned(),
            reason: preflight.fallback_reason,
        },
        Ok(preflight) => HotkeyCapability::Installable {
            planned_driver: preflight.planned_driver.to_owned(),
        },
    }
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
    fn canonical_hotkey_normalizes_outer_and_token_whitespace() {
        assert_eq!(canonical_hotkey("  ctrl + f9  "), "ctrl+f9");
        assert_eq!(canonical_hotkey("pause"), "pause");
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
