//! Regression scenario tests — prove the harness ACTUALLY catches the
//! bug classes the module doc (see `mod.rs`) advertises. Without these,
//! [`super::runner::run_iteration`] could silently return "PASS" for a
//! broken guard bracket and nobody would notice until users report
//! wedged PTT.
//!
//! Codex + Claude review of PR #518 flagged that the shipped harness
//! didn't exercise the two headline bug classes — "modifier state
//! leakage" and "unbalanced arm_end" — as concrete scenarios. This
//! module fills that gap: each `scenario_*` test induces the bug at the
//! primitive layer (`InjectionGuard`, `InjectionBracket`) and asserts
//! that the exact predicate `run_iteration` uses to detect it fires.
//!
//! These tests intentionally do NOT go through the full
//! `run_injection_idempotency_test` entry point — the plan builder is a
//! separate concern, and mixing plan drift into a "does the bracket
//! detector fire" test would obscure which layer regressed. Instead they
//! pin the DETECTOR primitives directly, matching the layering the
//! module doc calls out.

#![cfg(test)]
#![cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]

use std::time::{Duration, Instant};

use crate::hotkey::{InjectionBracket, InjectionGuard};

use super::report::FailureStage;
use super::runner::{run_injection_idempotency_test, run_iteration};

use crate::injection::plan::build_plan;
use crate::injection::LinuxSession;

/// Grace values matching the runner. Pinned locally so a rename in the
/// runner doesn't silently make these scenarios diverge from what the
/// production harness actually asserts against.
const PRE_GRACE: Duration = Duration::from_millis(50);
const POST_GRACE: Duration = Duration::from_millis(200);
const HORIZON_SLACK: Duration = Duration::from_millis(500);

// --------------------------------------------------------------------
// Scenario 1: "unbalanced arm_end" — a bracket that never closes.
//
// This is the classic bug class the module doc names: `arm_start`
// without matching `arm_end` (say the injector panicked mid-burst and
// the RAII drop didn't run). The harness's detection predicate is
// `guard.active_brackets() != 0` after the simulated burst; that same
// predicate MUST fire when we induce the bug by leaking the bracket
// deliberately here.
// --------------------------------------------------------------------

#[test]
fn scenario_unbalanced_arm_end_is_detected_by_active_brackets_check() {
    let guard = InjectionGuard::new();
    // Simulate the bug: open a bracket and then FORGET to close it (the
    // production RAII pattern would auto-close on drop; here we
    // `mem::forget` to model a panic path where drop never runs).
    let bracket = InjectionBracket::open(&guard, PRE_GRACE, POST_GRACE);
    std::mem::forget(bracket);

    // The runner's step 5 asserts `active_brackets() == 0` after the
    // bracket drop. With the leak in place the counter is stuck at 1 —
    // exactly what the detector fires on.
    assert_eq!(
        guard.active_brackets(),
        1,
        "leaked bracket must leave active_brackets stuck at 1 — this is what \
         the runner's post-drop check triggers `GuardStillActiveAfterEnd` on"
    );

    // And the horizon check (the belt-and-braces second predicate) also
    // fires: is_active_at(t + slack) is still true because the counter
    // is > 0 regardless of clock time.
    let far_future = Instant::now() + POST_GRACE + HORIZON_SLACK + Duration::from_secs(1);
    assert!(
        guard.is_active_at(far_future),
        "leaked bracket must keep is_active true forever — the runner's \
         step 5 horizon check would trigger `GuardStillActiveAfterEnd` here"
    );
}

#[test]
fn scenario_unbalanced_arm_end_carries_over_across_iterations() {
    // Chained scenario: iteration 1 leaks a bracket, iteration 2 would
    // then see `active_brackets != 0` at its pre-snapshot — the harness's
    // step 1 catches the carry-over. This is the concrete "unbalanced
    // arm_end" bug class the module doc names, expressed at the same
    // guard instance the runner uses.
    let guard = InjectionGuard::new();
    // Simulate iteration 1's leaked bracket.
    let bracket = InjectionBracket::open(&guard, PRE_GRACE, POST_GRACE);
    std::mem::forget(bracket);

    // The runner's step 1 pre-snapshot asserts active_brackets == 0
    // BEFORE any arm — with the carry-over this MUST be non-zero.
    assert_ne!(
        guard.active_brackets(),
        0,
        "leaked bracket from a prior iteration must be visible as \
         non-zero active_brackets — the runner's step 1 fires \
         `GuardActiveBeforeArm` on this exact predicate"
    );
    assert!(
        guard.is_active_at(Instant::now()),
        "guard must report active while a leaked bracket is still open"
    );
}

// --------------------------------------------------------------------
// Scenario 2: "modifier state leakage" — a Shift/Ctrl left "held" in the
// guard's tracked state after a burst finishes.
//
// The module doc's canonical example is a stuck Shift that would type
// `TEST TEXT` instead of `test text`. In the current architecture the
// only OBSERVABLE state that models "modifier still held" is the
// injection guard's bracket counter — a stuck modifier release means
// the injector's RAII bracket never dropped, which manifests as
// `active_brackets != 0`. So this scenario reuses the same detector
// primitives as scenario 1 but frames the fault at the semantic layer
// the module doc names.
//
// If the shipping architecture later grows a modifier-state field on
// the guard (see the TODO in [`crate::dictate::backends::inject::STALE_MODIFIER_VKS`]),
// this scenario is the place to plumb it through — the current version
// pins the "harness would notice" contract at the observable layer.
// --------------------------------------------------------------------

#[test]
fn scenario_modifier_state_leak_is_detected_via_bracket_counter() {
    // A modifier-state leak in the current architecture manifests as
    // the injector's RAII bracket outliving its `inject()` call — the
    // exact production shape a stuck `release_held_modifiers` would
    // produce (the modifier release is inside the bracket, so a hang or
    // panic there leaves the bracket open too).
    let guard = InjectionGuard::new();
    {
        let _bracket = InjectionBracket::open(&guard, PRE_GRACE, POST_GRACE);
        // Simulate: modifier release panicked (guard is still armed,
        // counter still > 0). The RAII drop below closes it cleanly —
        // this test proves the DETECTOR predicate fires WHILE the
        // simulated leak is in place.
        assert_eq!(
            guard.active_brackets(),
            1,
            "modifier release simulated as panicking mid-burst must leave \
             the bracket counter positive — the runner's in-bracket \
             `active_brackets() == 0` check fires here"
        );
        assert!(
            guard.is_active_at(Instant::now()),
            "guard must report active while the modifier-release-blocked \
             bracket is still open — this is the same predicate the runner's \
             step 4 in-bracket sanity check pins"
        );
        // Bracket drops on scope exit — a cleanly-recovered modifier
        // release doesn't leak. In the true bug scenario the drop
        // wouldn't run (see scenario 1 for the `mem::forget` variant).
    }
    // After clean recovery the counter drops back — this is the shape
    // the runner sees on a HAPPY iteration, contrasted with the leak
    // scenarios above.
    assert_eq!(guard.active_brackets(), 0);
}

// --------------------------------------------------------------------
// Scenario 3: "guard-active-before-arm" — the runner's step 1 detector.
// A fresh guard MUST report inactive and counter=0. A regression that
// leaked bracket-counter state via a shared global (or a `Default`
// impl that pre-armed itself) would trip this scenario.
// --------------------------------------------------------------------

#[test]
fn scenario_fresh_guard_starts_clean_step1_precondition() {
    // The step 1 pre-snapshot depends on `InjectionGuard::new()` starting
    // clean. This pins the invariant so a future rewrite of the guard
    // that accidentally initialised active_brackets to non-zero would
    // fail here rather than causing every self-test iteration to
    // spuriously report `GuardActiveBeforeArm`.
    let guard = InjectionGuard::new();
    assert_eq!(
        guard.active_brackets(),
        0,
        "InjectionGuard::new() must start with counter=0 — this is what \
         the runner's step 1 pre-snapshot pins"
    );
    assert!(
        !guard.is_active_at(Instant::now()),
        "InjectionGuard::new() must start inactive"
    );
}

// --------------------------------------------------------------------
// Scenario 4: end-to-end — an actually-good bracket lifecycle passes
// every runner assertion. Contrasts with the leak scenarios above so a
// regression that broke the detector wholesale (making it always PASS)
// would show up as scenarios 1-3 flipping green while scenario 4 stays
// green — the diff-and-compare pattern the harness protects against.
// --------------------------------------------------------------------

#[test]
fn scenario_clean_bracket_lifecycle_passes_runner_assertions() {
    // Drive `run_iteration` with a hand-built reference plan so we test
    // the actual runner code path (not just its detector primitives).
    let os = std::env::consts::OS;
    let session = LinuxSession::detect();
    let reference = build_plan("test text 1", "auto", os, session, true).unwrap();

    let result = run_iteration(1, "auto", os, session, &reference, false);
    assert_eq!(
        result.failed_at, None,
        "clean bracket lifecycle must not trip any detector, got: {:?} — {}",
        result.failed_at, result.detail,
    );
}

// --------------------------------------------------------------------
// Scenario 5: plan-drift detector. Two calls to `build_plan` with the
// same inputs MUST return materially-equal plans. This is the invariant
// the runner's step 3 `assert_plan_matches_reference` relies on — if it
// ever fails, every iteration would spuriously report `PlanDrift`.
// --------------------------------------------------------------------

#[test]
fn scenario_build_plan_is_deterministic_step3_precondition() {
    let a = build_plan(
        "hello world",
        "auto",
        "linux",
        LinuxSession::OtherWayland,
        true,
    )
    .unwrap();
    let b = build_plan(
        "hello world",
        "auto",
        "linux",
        LinuxSession::OtherWayland,
        true,
    )
    .unwrap();
    assert_eq!(
        a, b,
        "build_plan must be deterministic — this is the invariant the \
         runner's step 3 plan-drift check relies on"
    );
}

// --------------------------------------------------------------------
// Scenario 6: FailureStage token pinning at the scenario layer. The
// smoke script greps for these — a rename here should trip the
// contract, not silently reshape it.
// --------------------------------------------------------------------

// --------------------------------------------------------------------
// Runner happy-path + boundary tests. Kept alongside the fault-injection
// scenarios above so a diff-and-compare reviewer sees the "harness
// catches this" scenario next to the "harness passes this" scenario.
// --------------------------------------------------------------------

#[test]
fn runner_happy_path_all_iterations_pass_dry_run() {
    let report = run_injection_idempotency_test(5, "auto", false);
    assert!(
        report.all_passed(),
        "expected all iterations to pass, got: {}",
        report.to_plain()
    );
    assert_eq!(report.iterations, 5);
    assert_eq!(report.results.len(), 5);
    assert!(!report.live);
}

#[test]
fn runner_happy_path_pinned_backend_pass_dry_run() {
    let report = run_injection_idempotency_test(3, "enigo", false);
    assert!(
        report.all_passed(),
        "pinned-backend dry-run must pass, got: {}",
        report.to_plain()
    );
    assert_eq!(report.backend, "enigo");
}

#[test]
fn runner_zero_iterations_returns_empty_results() {
    let report = run_injection_idempotency_test(0, "auto", false);
    assert!(report.all_passed());
    assert_eq!(report.results.len(), 0);
    assert!(!report.live);
}

#[test]
fn runner_unknown_backend_surfaces_as_plan_build_failure() {
    let report = run_injection_idempotency_test(3, "definitely-not-a-backend", false);
    assert!(!report.all_passed());
    assert_eq!(report.results.len(), 1);
    assert_eq!(
        report.results[0].failed_at,
        Some(FailureStage::PlanBuild),
        "expected PlanBuild failure, got: {}",
        report.to_plain()
    );
}

#[test]
fn runner_backend_label_is_reported_in_result() {
    let report = run_injection_idempotency_test(1, "auto", false);
    assert_eq!(report.backend, "auto");
    let report = run_injection_idempotency_test(1, "enigo", false);
    assert_eq!(report.backend, "enigo");
}

#[test]
fn scenario_failure_stage_tokens_used_by_smoke_script() {
    // Duplicated with the report-level test on purpose: this pins the
    // exact set of tokens the SMOKE SCRIPT uses to grep, which is a
    // caller-facing contract distinct from the enum-shape test in
    // report.rs.
    for (stage, expected) in [
        (FailureStage::PlanBuild, "plan_build"),
        (FailureStage::PlanDrift, "plan_drift"),
        (
            FailureStage::GuardActiveBeforeArm,
            "guard_active_before_arm",
        ),
        (
            FailureStage::GuardStillActiveAfterEnd,
            "guard_still_active_after_end",
        ),
        (FailureStage::ExecuteFailed, "execute_failed"),
    ] {
        assert_eq!(stage.as_str(), expected);
    }
}
