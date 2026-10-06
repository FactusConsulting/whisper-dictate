use super::*;

// -----------------------------------------------------------------------
// Modifier VK groups (release chord when modifier
// released mid-hold).
//
// The release-polling path in `run_msg_loop` treats the chord as
// released as soon as ANY required modifier family stops registering as
// down — so `ctrl+f9` fires ChordRelease when the user lets go of Ctrl
// while still holding F9. The pure `required_modifier_vk_groups` helper
// selects the VKs to poll; test it directly since GetAsyncKeyState is
// not driveable from a unit test.
// -----------------------------------------------------------------------

// Windows VK constants (same as the production module's private consts).
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

#[test]
fn required_modifier_vk_groups_is_empty_for_no_mods() {
    // A chord with no modifiers (e.g. bare `f9`) has nothing to poll:
    // the trigger VK is the sole liveness signal.
    assert!(required_modifier_vk_groups(0).is_empty());
}

#[test]
fn required_modifier_vk_groups_maps_each_family_to_its_vk() {
    // Each MOD_* bit maps to the generic VK for its family. Control
    // Shift / Alt each have a single-VK group; Win has BOTH LWIN and
    // RWIN because Windows has no unified Win VK.
    assert_eq!(
        required_modifier_vk_groups(MOD_CONTROL),
        vec![vec![VK_CONTROL]]
    );
    assert_eq!(required_modifier_vk_groups(MOD_SHIFT), vec![vec![VK_SHIFT]]);
    assert_eq!(required_modifier_vk_groups(MOD_ALT), vec![vec![VK_MENU]]);
    assert_eq!(
        required_modifier_vk_groups(MOD_WIN),
        vec![vec![VK_LWIN, VK_RWIN]]
    );
}

// -----------------------------------------------------------------------
// plan_register — the "validate BEFORE unregister" contract (
// Verify modifier release handling.
//
// The message loop's Register handler now calls `plan_register` before
// touching any OS state. A parse failure must be surfaced as
// `RegisterPlan::Reject` so `handle_command` acks the error WITHOUT
// unregistering the previously-working binding — the guarantee that
// keeps the process from ending up with no listener at all when the
// supervisor sends a bad new chord (e.g. the user typed a side-specific
// alias into settings and asked for a resume-with-new-chord).
// -----------------------------------------------------------------------

#[test]
fn required_modifier_vk_groups_composes_multiple_families() {
    // Ctrl+Shift+F9 style chord: both families must poll their VKs.
    // Order mirrors the bit-check order in the helper (Control, Shift,
    // Alt, Win) so downstream consumers can rely on a stable
    // enumeration for logging.
    assert_eq!(
        required_modifier_vk_groups(MOD_CONTROL | MOD_SHIFT),
        vec![vec![VK_CONTROL], vec![VK_SHIFT]]
    );
    assert_eq!(
        required_modifier_vk_groups(MOD_CONTROL | MOD_SHIFT | MOD_ALT | MOD_WIN),
        vec![
            vec![VK_CONTROL],
            vec![VK_SHIFT],
            vec![VK_MENU],
            vec![VK_LWIN, VK_RWIN],
        ]
    );
}
