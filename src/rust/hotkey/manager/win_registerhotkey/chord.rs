//! chord policy for the Windows RegisterHotKey driver.

use super::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

/// A user's PTT chord parsed into the (modifier mask, trigger VK) shape
/// `RegisterHotKey` accepts, plus the trigger's canonical name so we can
/// tee it into the diagnostic log line the caller reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedChord {
    pub mods: u32,
    pub vk: u32,
    pub trigger_name: String,
    /// Human-readable form (`"ctrl+shift+f9"`) for diagnostic lines. Not
    /// used by the register call itself.
    pub display: String,
}

/// Parse a `+`-separated list of PTT key names into the RegisterHotKey
/// shape. Accepts every name the rdev driver accepts (see
/// `rdev_driver::RDEV_SUPPORTED_NAMES`) plus the modifier-family aliases
/// (`ctrl` → MOD_CONTROL, `alt_gr` → MOD_ALT, ...). Rejects:
///
/// * **modifier-only chords** — RegisterHotKey requires exactly one
///   non-modifier trigger VK. This is the driver's single hard
///   limitation vs. the rdev backend and is surfaced with an
///   actionable error message so the caller can fall back or ask the
///   user to reconfigure.
/// * **multiple trigger keys** — only one VK per registration; a
///   chord like `f9+f10` is rejected explicitly.
/// * **unsupported trigger names** — anything outside the accepted
///   set (function keys, `space`, `esc`, `tab`, `enter`, and the
///   printable ASCII keys the OS maps 1:1 to a VK).
///
/// The name → VK table intentionally mirrors `rdev_driver::key_to_name`
/// so a chord that installs cleanly on rdev also installs cleanly on
/// this driver — modulo the modifier-only limitation. Names outside
/// that table produce an error that names the offending segment.
pub fn parse_chord(names: &[String]) -> Result<ParsedChord, String> {
    if names.is_empty() {
        return Err("chord is empty".to_owned());
    }
    let mut mods: u32 = 0;
    let mut trigger: Option<(u32, String)> = None;
    let mut segments: Vec<String> = Vec::with_capacity(names.len());
    for raw in names {
        let name = raw.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        segments.push(name.clone());
        // Side-specific modifier aliases (`ctrl_l`, `alt_r`, …) cannot be
        // honoured faithfully by RegisterHotKey: the OS `MOD_*` flags
        // trigger on EITHER side of the modifier family, so a
        // `ctrl_r+f9` binding registered as `MOD_CONTROL|VK_F9` would
        // also fire for `ctrl_l+f9` — the opposite of what the user
        // configured. Reject side-specific names at parse time so the
        // supervisor's install path falls back to rdev (which the
        // tracker does track per-side accurately).
        if is_side_specific_modifier(&name) {
            return Err(format!(
                "chord key {name:?} names a side-specific modifier that \
                 the Windows RegisterHotKey driver cannot honour \
                 (MOD_* flags fire on either side of the modifier \
                 family). Use the generic name (`ctrl` / `shift` / \
                 `alt` / `win`) if either side is acceptable, or set \
                 VOICEPI_HOTKEY_DRIVER=rdev to keep the side-specific \
                 binding."
            ));
        }
        if let Some(bit) = modifier_bit(&name) {
            mods |= bit;
            continue;
        }
        let vk = vk_from_trigger_name(&name).ok_or_else(|| {
            format!(
                "chord key {name:?} is not supported by the Windows \
                 RegisterHotKey driver (accepted triggers: f1-f12, space, \
                 esc, tab, enter, and single ASCII letter/digit keys)"
            )
        })?;
        if let Some((_, existing_name)) = &trigger {
            return Err(format!(
                "chord has more than one non-modifier key ({existing_name} and {name}); \
                 RegisterHotKey supports at most one trigger VK per chord"
            ));
        }
        trigger = Some((vk, name));
    }
    let (vk, trigger_name) = trigger.ok_or_else(|| {
        "chord contains only modifiers; the Windows RegisterHotKey driver \
         requires at least one non-modifier trigger key (function key, \
         letter, digit, space, escape, tab, or enter). Set \
         VOICEPI_HOTKEY_DRIVER=rdev to keep a modifier-only binding, \
         or add a trigger key to the chord."
            .to_owned()
    })?;
    let display = segments.join("+");
    Ok(ParsedChord {
        mods,
        vk,
        trigger_name,
        display,
    })
}

/// Modifier name → `MOD_*` flag mapping. Accepts every alias the tracker
/// / rdev driver accepts so a chord that parses on either of those
/// parses here too (except for the modifier-only case rejected in
/// [`parse_chord`] and the side-specific-alias case which [`parse_chord`]
/// rejects up-front via [`is_side_specific_modifier`]).
fn modifier_bit(name: &str) -> Option<u32> {
    match name {
        "ctrl" | "ctrl_l" | "ctrl_r" => Some(MOD_CONTROL),
        "shift" | "shift_l" | "shift_r" => Some(MOD_SHIFT),
        // alt_gr / right_alt / ralt map to Alt too — the OS reports
        // AltGr as Ctrl+Alt, but the Windows MOD_ALT flag DOES fire on
        // the right-Alt key in the common European-layout setup.
        "alt" | "alt_l" | "alt_r" | "alt_gr" | "right_alt" | "ralt" => Some(MOD_ALT),
        "cmd" | "cmd_l" | "cmd_r" | "win" | "win_l" | "win_r" => Some(MOD_WIN),
        _ => None,
    }
}

/// True when `name` is a modifier alias that names a specific side
/// (left vs. right) of the key. `RegisterHotKey`'s `MOD_*` flags fire on
/// EITHER side, so a side-specific binding cannot be honoured faithfully:
/// registering `ctrl_r+f9` as `MOD_CONTROL|VK_F9` would also fire for
/// `ctrl_l+f9`. [`parse_chord`] uses this to reject the chord up-front so
/// the supervisor's install path falls back to rdev (which tracks
/// modifier sides accurately).
///
/// Includes `alt_gr` / `right_alt` / `ralt` because those all name the
/// right-Alt key specifically on European layouts; a user asking for
/// AltGr in their chord does NOT want left-Alt to also trigger it.
pub(crate) fn is_side_specific_modifier(name: &str) -> bool {
    matches!(
        name,
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

/// Trigger name → Windows virtual-key code. Deliberately limited to the
/// keys the rdev driver already accepts as triggers, so smoke scripts
/// don't have to distinguish "rdev-only" from "register-only" chords.
/// Letters and digits are accepted via ASCII passthrough (the VK for
/// `A`..`Z` is the ASCII byte itself; same for `0`..`9`).
pub(crate) fn vk_from_trigger_name(name: &str) -> Option<u32> {
    let vk: u32 = match name {
        "f1" => 0x70,
        "f2" => 0x71,
        "f3" => 0x72,
        "f4" => 0x73,
        "f5" => 0x74,
        "f6" => 0x75,
        "f7" => 0x76,
        "f8" => 0x77,
        "f9" => 0x78,
        "f10" => 0x79,
        "f11" => 0x7A,
        "f12" => 0x7B,
        "space" => 0x20,
        "esc" => 0x1B,
        "tab" => 0x09,
        "enter" => 0x0D,
        "pause" => 0x13,
        other => {
            let bytes = other.as_bytes();
            if bytes.len() == 1 {
                let b = bytes[0];
                if b.is_ascii_alphabetic() {
                    b.to_ascii_uppercase() as u32
                } else if b.is_ascii_digit() {
                    b as u32
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    };
    Some(vk)
}
