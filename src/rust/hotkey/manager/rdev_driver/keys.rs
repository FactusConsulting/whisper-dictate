//! Raw-key conversion and supported binding names.

use super::super::tracker::{RawKeyEvent, RawKeyKind};
use std::time::Instant;

/// Convert an `rdev::Event` into the platform-agnostic [`RawKeyEvent`] the
/// tracker consumes. Returns `None` only for non-keyboard events (mouse,
/// etc.); unknown key variants get a synthetic `__rdev_<Debug>` name so the
/// tracker can still detect foreign-key holds for bare-modifier rule 1/2
/// . PTT-target matching never collides with these names
/// since every PTT-able name is in `key_to_name`.
///
/// `pub(crate)` so the companion `rdev_driver_tests.rs` can drive it with
/// synthetic events without a shim.
pub(crate) fn raw_from_rdev(event: &rdev::Event) -> Option<RawKeyEvent> {
    let (key, kind) = match event.event_type {
        rdev::EventType::KeyPress(k) => (k, RawKeyKind::Press),
        rdev::EventType::KeyRelease(k) => (k, RawKeyKind::Release),
        _ => return None,
    };
    let name = key_to_name(key).unwrap_or_else(|| format!("__rdev_{key:?}"));
    Some(RawKeyEvent {
        name,
        kind,
        at: Instant::now(),
    })
}

/// Names the rdev driver can actually translate into [`RawKeyEvent`]s. A
/// PTT binding whose name isn't in this set silently never fires — see
/// [`is_rdev_supported_name`] for the install-time validator that rejects
/// such bindings up front (P2 finding #6).
const RDEV_SUPPORTED_NAMES: &[&str] = &[
    "ctrl_l",
    "ctrl_r",
    "ctrl",
    "shift_l",
    "shift_r",
    "shift",
    "alt_l",
    // `alt_r` is a valid side-specific binding: `modifier_family` classifies
    // it as `alt`, `canonical_side("alt_gr") == canonical_side("alt_r")`, and
    // an OS-delivered `alt_gr` / `right_alt` press satisfies the binding.
    // Without it in this list, install-time validation rejected `alt_r+...`
    // chords the moment RegisterHotKey rejected them for being side-specific
    // (the rdev fallback path).
    "alt_r",
    "alt",
    "alt_gr",
    // `right_alt` and `ralt` are accepted aliases for `alt_gr` / AltGr
    // : rdev maps both to K::AltGr → "alt_gr" via
    // `key_to_name`, and `modifier_family` / `canonical_side` treat them
    // as equivalent to `alt_r`, so the tracker matches correctly.
    "right_alt",
    "ralt",
    "cmd_l",
    "cmd_r",
    "cmd",
    // Windows-terminology aliases for the Meta / Super key family
    // (rdev emits `cmd_l` / `cmd_r`). `modifier_family` / `canonical_side`
    // treat these as `cmd`-family equivalents so a `win_l+f9` binding
    // (rejected by RegisterHotKey as side-specific) reaches the
    // rdev fallback and matches real Meta-key presses.
    "win",
    "win_l",
    "win_r",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "space",
    "esc",
    "tab",
    "enter",
    "pause",
];

/// True if `name` is one of the PTT-binding names the rdev driver can
/// translate. Used by the hotkey installer to reject (or remap) names
/// like `super_l` / `super_r` that the alternate driver cannot translate.
/// The generic `ctrl` / `shift` / `alt` / `cmd` are
/// included because they are valid PTT *bindings* even though rdev never
/// emits them as raw events — `modifier_matches` handles the matching, and
/// rule 1 / 2 still need to know they're targets.
pub fn is_rdev_supported_name(name: &str) -> bool {
    RDEV_SUPPORTED_NAMES.contains(&name)
}

/// Map `rdev::Key` to the lowercase-name convention used by the PTT
/// settings (`ctrl_l`, `shift_r`, `alt_gr`, `f9`, single chars, ...).
/// Unmapped keys return `None` — they cannot be a PTT target so silently
/// dropping them is fine.
pub(super) fn key_to_name(key: rdev::Key) -> Option<String> {
    use rdev::Key as K;
    let name = match key {
        K::ControlLeft => "ctrl_l",
        K::ControlRight => "ctrl_r",
        K::ShiftLeft => "shift_l",
        K::ShiftRight => "shift_r",
        K::Alt => "alt_l",
        K::AltGr => "alt_gr",
        K::MetaLeft => "cmd_l",
        K::MetaRight => "cmd_r",
        K::F1 => "f1",
        K::F2 => "f2",
        K::F3 => "f3",
        K::F4 => "f4",
        K::F5 => "f5",
        K::F6 => "f6",
        K::F7 => "f7",
        K::F8 => "f8",
        K::F9 => "f9",
        K::F10 => "f10",
        K::F11 => "f11",
        K::F12 => "f12",
        K::Space => "space",
        K::Escape => "esc",
        K::Tab => "tab",
        K::Return => "enter",
        K::Pause => "pause",
        _ => return None,
    };
    Some(name.to_owned())
}
