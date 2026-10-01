use super::*;

#[test]
fn plan_register_accepts_valid_chord() {
    let plan = plan_register(&s(&["ctrl", "f9"]));
    match plan {
        RegisterPlan::Install(chord) => {
            assert_eq!(chord.mods, MOD_CONTROL);
            assert_eq!(chord.vk, 0x78);
        }
        RegisterPlan::Reject(msg) => panic!("expected Install, got Reject({msg:?})"),
    }
}

#[test]
fn plan_register_rejects_side_specific_before_state_touch() {
    // The P1 fix's core invariant: a side-specific chord (which the
    // parser now rejects) must produce Reject so `handle_command`
    // returns without calling `unregister_current`. If a future
    // refactor moved the OS unregister ahead of the plan gate, this
    // test would still pass — the ordering discipline is enforced by
    // the test below (`register_reject_leaves_previous_binding_intact`).
    let plan = plan_register(&s(&["ctrl_l", "f9"]));
    assert!(
        matches!(plan, RegisterPlan::Reject(_)),
        "side-specific chord must plan to Reject: {plan:?}"
    );
}

#[test]
fn plan_register_rejects_modifier_only_chord() {
    let plan = plan_register(&s(&["ctrl"]));
    assert!(
        matches!(plan, RegisterPlan::Reject(_)),
        "modifier-only chord must plan to Reject: {plan:?}"
    );
}

#[test]
fn plan_register_rejects_empty_chord() {
    let plan = plan_register(&[]);
    assert!(
        matches!(plan, RegisterPlan::Reject(_)),
        "empty chord must plan to Reject: {plan:?}"
    );
}
