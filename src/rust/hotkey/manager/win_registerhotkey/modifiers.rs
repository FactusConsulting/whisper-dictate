//! modifiers policy for the Windows RegisterHotKey driver.

use super::{async_key_down, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

// Windows virtual-key codes for the generic modifier keys (either side).
// Exported at module scope so the smoke test / release-polling helper can
// share the values with the parse layer.
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12; // Alt
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

/// Collect the VKs that must be down for the chord's `MOD_*` mask to be
/// considered "still held". `MOD_WIN` has no unified VK so BOTH LWIN and
/// RWIN must be inspected; if either is down, the Win-family requirement
/// is satisfied — hence the return shape is a list of "any-of" groups.
///
/// Pure helper: the modifier-release logic is unit-testable without a
/// real Windows message loop.
pub(crate) fn required_modifier_vk_groups(mods: u32) -> Vec<Vec<u32>> {
    let mut groups: Vec<Vec<u32>> = Vec::new();
    if mods & MOD_CONTROL != 0 {
        groups.push(vec![VK_CONTROL]);
    }
    if mods & MOD_SHIFT != 0 {
        groups.push(vec![VK_SHIFT]);
    }
    if mods & MOD_ALT != 0 {
        groups.push(vec![VK_MENU]);
    }
    if mods & MOD_WIN != 0 {
        // Win-family: no unified VK, poll BOTH sides.
        groups.push(vec![VK_LWIN, VK_RWIN]);
    }
    groups
}

/// True iff every modifier family named in `mods` has at least one of its
/// VKs currently held. Consulted from the release-polling path so a
/// mid-hold modifier release fires the chord release even while the
/// trigger key is still down.
pub(super) fn required_modifiers_down(mods: u32) -> bool {
    required_modifier_vk_groups(mods)
        .into_iter()
        .all(|group| group.into_iter().any(async_key_down))
}
