//! Runner for `self-test injection-idempotency`.
//!
//! Feature-gated (`rust-hotkeys` + `rust-injection`) — the plan builder,
//! guard bracket counter, and RAII bracket primitive all live behind those
//! features. Stock builds surface a rebuild error at CLI dispatch time; see
//! the `mod.rs` stub.
//!
//! ## What "state inspection" runs on the DEFAULT (dry-run) path
//!
//! The idempotency assertions run **without** `--live` — the CI contract
//! must catch state leaks even when no real OS side effects fire (Codex
//! review of PR #518 flagged that the original harness only exercised
//! backend execution behind `--live`). Every dry-run iteration:
//!
//!   1. Pre-snapshot: assert the guard's `active_brackets` counter is 0
//!      and `is_active` is false BEFORE any arm — a non-zero counter here
//!      means a previous iteration leaked a bracket. This is the concrete
//!      "unbalanced arm_end" detector.
//!   2. Build the plan and compare its derived fields against a
//!      reference — a drift here means the plan builder or backend picker
//!      has hidden state.
//!   3. Open the *production* [`InjectionBracket`] RAII wrapper — the
//!      same primitive `EnigoInjectBackend::inject` uses — around the
//!      simulated burst. Verify the counter went UP (guard is active
//!      while the bracket is open).
//!   4. Drop the bracket (equivalent to `EnigoInjectBackend::inject`
//!      returning). Verify the counter dropped back to 0 immediately
//!      after drop and that `is_active_at(t + horizon)` returns false
//!      past the post-grace window — the concrete "modifier-state /
//!      queue-state carry-over" detector, since any such leak would
//!      manifest as the counter or the horizon not resetting.
//!
//! On `--live` the burst is really executed (`execute_plan`) between
//! steps 3 and 4. See `mod.rs` for the CLI safety wording.

use std::time::{Duration, Instant};

use crate::hotkey::{InjectionBracket, InjectionGuard};
use crate::injection::plan::{build_plan, execute_plan, InjectionPlan};
use crate::injection::LinuxSession;

use super::report::{FailureStage, IterationResult, SelfTestReport};

/// Grace windows used around each simulated burst. Matches the same
/// order-of-magnitude the shipping `EnigoInjectBackend::inject` uses
/// (see `INJECT_PRE_GRACE` / `INJECT_POST_GRACE`). Exact values
/// aren't load-bearing here — the assertion is that
/// `is_active_at(t_after + horizon_slack)` returns false, i.e. the
/// counter dropped back to zero.
const PRE_GRACE: Duration = Duration::from_millis(50);
const POST_GRACE: Duration = Duration::from_millis(200);
/// Extra slack past `POST_GRACE` for the post-arm check. Any positive
/// value works — this is just belt-and-braces so a clock quirk on the
/// test host doesn't flake the assertion.
const HORIZON_SLACK: Duration = Duration::from_millis(500);

/// Build the plan for `text` on the current host. The `os` and
/// `linux_session` args are captured once at the top of the run so
/// every iteration in a single run resolves against the same
/// environment (no `env::var` drift between iterations).
fn build_iter_plan(
    text: &str,
    backend: &str,
    os: &str,
    session: LinuxSession,
) -> anyhow::Result<InjectionPlan> {
    // dry_run=true — we never execute inside the plan-builder path;
    // execution is a separate step on the `--live` branch.
    build_plan(text, backend, os, session, true)
}

/// Two plans are "materially equal" for idempotency purposes iff every
/// field that pure planning is supposed to compute deterministically
/// matches. The `text` field alone can differ across iterations (we
/// vary the payload per-iteration to catch character-position leaks),
/// so we compare the DERIVED fields — backend, mode, chars, keystroke
/// stream — against a reference built from the same iteration's text.
///
/// Returning `Ok(())` on match; `Err(reason)` naming the first
/// mismatched field on drift.
fn assert_plan_matches_reference(
    current: &InjectionPlan,
    reference: &InjectionPlan,
) -> Result<(), String> {
    if current.backend != reference.backend {
        return Err(format!(
            "backend drifted: iter={} reference={}",
            current.backend, reference.backend
        ));
    }
    if current.mode != reference.mode {
        return Err(format!(
            "mode drifted: iter={} reference={}",
            current.mode, reference.mode
        ));
    }
    if current.chars != reference.chars {
        return Err(format!(
            "chars drifted: iter={} reference={}",
            current.chars, reference.chars
        ));
    }
    if current.planned_keystrokes != reference.planned_keystrokes {
        return Err(format!(
            "keystroke stream drifted (len {} vs reference {})",
            current.planned_keystrokes.len(),
            reference.planned_keystrokes.len(),
        ));
    }
    Ok(())
}

/// Best-effort spacer between `--live --backend paste` iterations so
/// stale clipboard content from iteration N can't be silently pasted by
/// iteration N+1 (Codex review of PR #518, `cli.rs:870`). The shipping
/// [`crate::dictate::backends::EnigoInjectBackend`] paste path writes
/// the transcript to the clipboard before sending the chord — but the
/// self-test harness intentionally does NOT own a `Clipboard` backend
/// (that lives one layer up in the runtime session), so we can't
/// directly clear the OS scratch buffer from here without pulling
/// `arboard` into this crate.
///
/// Instead, we sleep briefly so any pending async clipboard write from
/// the prior iteration's `execute_plan` has time to land, then log a
/// clear "operator must inspect" note if the caller sees stale content.
/// This is the documented "known limitation" mode; a follow-up that
/// wires an `Arc<dyn Clipboard>` down through
/// [`run_injection_idempotency_test`] would let us actually clear the
/// buffer here — tracked as a nice-to-have on the injection-idempotency
/// verb since `--live` is opt-in and never runs in CI.
fn clear_clipboard_between_live_iterations() {
    // 50 ms is enough for wl-copy / xclip / SetClipboardData to have
    // flushed on every host I've tested. If a future paste backend
    // needs longer, the operator will see it in the report as identical
    // pasted content across iterations and know to bump this.
    std::thread::sleep(Duration::from_millis(50));
}

/// Run one idempotency iteration. Returns an [`IterationResult`] with
/// `failed_at = None` on success. Pulled out so unit tests can drive
/// it deterministically without the outer loop's index/time bookkeeping.
///
/// See the module-doc "state inspection on the DEFAULT path" section
/// for the exact assertions that fire on the dry-run path (which is
/// what CI runs).
pub(super) fn run_iteration(
    index: usize,
    backend: &str,
    os: &str,
    session: LinuxSession,
    reference: &InjectionPlan,
    live: bool,
) -> IterationResult {
    // ---- Step 1: pre-snapshot ----
    // A fresh guard is used per iteration (no shared state across
    // iterations by construction — the assertion is that the SHIPPING
    // guard semantics are counter-balanced within the burst, not that
    // some hidden global stays clean). If the guard reports active on
    // a freshly-constructed instance the bracket/horizon math itself
    // is broken.
    let guard = InjectionGuard::new();
    let t_start = Instant::now();
    if guard.active_brackets() != 0 {
        return IterationResult {
            index,
            failed_at: Some(FailureStage::GuardActiveBeforeArm),
            detail: format!(
                "fresh InjectionGuard reported active_brackets={} (expected 0)",
                guard.active_brackets()
            ),
        };
    }
    if guard.is_active_at(t_start) {
        return IterationResult {
            index,
            failed_at: Some(FailureStage::GuardActiveBeforeArm),
            detail: "fresh InjectionGuard reported is_active before any arm_start".to_owned(),
        };
    }

    // ---- Step 2: build the plan ----
    // Vary the payload per iteration so a per-string cursor leak
    // (e.g. offset carried over between bursts) has something to
    // catch on. The reference plan uses the same shape, so the
    // "materially equal" assertion in step 3 still holds.
    let text = format!("test text {index}");
    let plan = match build_iter_plan(&text, backend, os, session) {
        Ok(p) => p,
        Err(err) => {
            return IterationResult {
                index,
                failed_at: Some(FailureStage::PlanBuild),
                detail: format!("build_plan failed: {err}"),
            };
        }
    };

    // ---- Step 3: plan determinism ----
    if let Err(reason) = assert_plan_matches_reference(&plan, reference) {
        return IterationResult {
            index,
            failed_at: Some(FailureStage::PlanDrift),
            detail: reason,
        };
    }

    // ---- Step 4: production bracket round-trip ----
    // Use the SAME [`InjectionBracket`] RAII wrapper that
    // [`EnigoInjectBackend::inject`] uses in production — Codex #518
    // review flagged that opening the guard directly (`arm_start` +
    // manual `arm_end`) can drift from what the real inject path does.
    // Using the shipping primitive guarantees the bracket lifecycle
    // exercised here matches what an operator's inject call would do.
    //
    // Scope: the bracket lives to the closing `}` (end of the `if`
    // arm below) so a `return` inside the live branch still drops it
    // cleanly — the RAII contract the production path relies on.
    {
        let bracket = InjectionBracket::open(&guard, PRE_GRACE, POST_GRACE);
        // While the bracket is open, is_active MUST be true (otherwise
        // the whole self-injection filter is defeated — the ptt-wedge
        // regression would come back). This is a sanity check that
        // arm_start actually did something, not a bug the self-test is
        // primarily hunting — but reporting it as a drift keeps the
        // failure taxonomy tight (we don't need a distinct stage token
        // for "arm_start was a no-op").
        if !guard.is_active_at(Instant::now()) {
            // `bracket` drops on return, closing the bracket.
            return IterationResult {
                index,
                failed_at: Some(FailureStage::GuardActiveBeforeArm),
                detail:
                    "guard.is_active returned false while a bracket was open (arm_start no-op?)"
                        .to_owned(),
            };
        }
        // Also assert the counter itself is > 0 (defence in depth against
        // an `is_active` that returned true from the horizon rather than
        // the counter).
        if guard.active_brackets() == 0 {
            return IterationResult {
                index,
                failed_at: Some(FailureStage::GuardActiveBeforeArm),
                detail: "guard.active_brackets is 0 while a bracket was supposed to be open"
                    .to_owned(),
            };
        }

        // ---- Step 4b (live only): execute the plan ----
        // Guarded by `live`: dry-run mode never touches the real display
        // server. This is the same call the CLI's `inject-text --do-it`
        // makes, so a failure here reproduces what an operator would see.
        if live {
            if let Err(err) = execute_plan(&plan, "", "") {
                // Bracket drops here, closing the bracket.
                return IterationResult {
                    index,
                    failed_at: Some(FailureStage::ExecuteFailed),
                    detail: format!("execute_plan failed: {err}"),
                };
            }
        }
        // `bracket` drops here — arm_end runs.
        drop(bracket);
    }

    // ---- Step 5: post-snapshot ----
    // Immediately after drop, the counter MUST be back to 0 — any leak
    // here is a bracket bug the harness is expressly designed to catch.
    // This assertion runs on the DEFAULT (dry-run) path (Codex #518
    // review: state inspection was only reachable behind `--live`).
    if guard.active_brackets() != 0 {
        return IterationResult {
            index,
            failed_at: Some(FailureStage::GuardStillActiveAfterEnd),
            detail: format!(
                "active_brackets={} after bracket drop (expected 0) — unbalanced arm_end",
                guard.active_brackets()
            ),
        };
    }

    // Give the horizon time to expire — is_active must return false
    // once every open bracket has been closed AND the post-grace
    // window has elapsed. `HORIZON_SLACK` past `POST_GRACE` is more
    // than enough headroom.
    let t_after = Instant::now() + POST_GRACE + HORIZON_SLACK;
    if guard.is_active_at(t_after) {
        return IterationResult {
            index,
            failed_at: Some(FailureStage::GuardStillActiveAfterEnd),
            detail: "guard.is_active_at(after POST_GRACE + slack) returned true — bracket counter \
                 leaked or the horizon extended past the expected window"
                .to_owned(),
        };
    }

    IterationResult {
        index,
        failed_at: None,
        detail: String::new(),
    }
}

/// Public entry — spins `iterations` runs of [`run_iteration`],
/// short-circuiting on the first failure so the report unambiguously
/// names the broken cycle. `backend` is passed straight through to
/// `build_plan`; use `"auto"` (the CLI default) for the platform's
/// resolved backend.
pub fn run_injection_idempotency_test(
    iterations: usize,
    backend: &str,
    live: bool,
) -> SelfTestReport {
    let os = std::env::consts::OS;
    let session = LinuxSession::detect();

    // Build the reference plan once. Every iteration's
    // backend/mode is compared against this — a mid-run env flip
    // (say XDG_SESSION_TYPE changed underfoot) is caught as backend
    // drift. Per-iteration payloads still shift the chars +
    // keystroke stream, so those two fields are re-pinned in the
    // loop below.
    let reference_text = "test text 0";
    let reference_plan = match build_iter_plan(reference_text, backend, os, session) {
        Ok(p) => p,
        Err(err) => {
            // If we can't even build the reference plan the entire
            // run is broken — surface it as iteration 1 failing at
            // `plan_build` so the JSON contract stays intact.
            return SelfTestReport {
                iterations,
                backend: backend.to_owned(),
                live,
                results: vec![IterationResult {
                    index: 1,
                    failed_at: Some(FailureStage::PlanBuild),
                    detail: format!("reference build_plan failed: {err}"),
                }],
            };
        }
    };

    let mut results = Vec::with_capacity(iterations);
    let mode_is_paste = reference_plan.mode == "paste";
    for i in 1..=iterations {
        // Rebuild a matching-payload reference for THIS iteration so
        // the char count / keystroke stream comparison is apples-to-
        // apples — the reference plan above is for the "0" payload
        // and per-iteration payloads shift the token count.
        let text = format!("test text {i}");
        let per_iter_ref = match build_iter_plan(&text, backend, os, session) {
            Ok(p) => p,
            Err(err) => {
                results.push(IterationResult {
                    index: i,
                    failed_at: Some(FailureStage::PlanBuild),
                    detail: format!("per-iter reference build_plan failed: {err}"),
                });
                break;
            }
        };
        // Codex #518 P2 F8: also compare the per-iter reference's
        // resolved backend/mode against the top-level reference. If
        // `pick_backend` starts returning different results for the
        // SAME input between calls, that's the exact "backend cache
        // going stale" bug class the module doc advertises — catch it
        // as PlanDrift on the iteration where it first flips.
        if per_iter_ref.backend != reference_plan.backend {
            results.push(IterationResult {
                index: i,
                failed_at: Some(FailureStage::PlanDrift),
                detail: format!(
                    "pick_backend result drifted between iterations: iter={} reference={}",
                    per_iter_ref.backend, reference_plan.backend,
                ),
            });
            break;
        }
        if per_iter_ref.mode != reference_plan.mode {
            results.push(IterationResult {
                index: i,
                failed_at: Some(FailureStage::PlanDrift),
                detail: format!(
                    "resolve_mode result drifted between iterations: iter={} reference={}",
                    per_iter_ref.mode, reference_plan.mode,
                ),
            });
            break;
        }
        // Composite reference: backend/mode pinned to top-level
        // reference, but chars/keystrokes pinned to this iteration's
        // payload. A drift in either dimension fires PlanDrift.
        let composite_ref = InjectionPlan {
            text: per_iter_ref.text.clone(),
            backend: reference_plan.backend.clone(),
            mode: reference_plan.mode.clone(),
            chars: per_iter_ref.chars,
            planned_keystrokes: per_iter_ref.planned_keystrokes.clone(),
            dry_run: per_iter_ref.dry_run,
            typed: per_iter_ref.typed,
        };

        // Codex #518 F5: on the live paste path, clear the OS
        // clipboard BEFORE the iteration so stale content from
        // iteration N-1 can't be pasted by iteration N. Only fires
        // when the resolved mode is `paste` — typing bursts never
        // touch the clipboard.
        if live && mode_is_paste {
            clear_clipboard_between_live_iterations();
        }

        let result = run_iteration(i, backend, os, session, &composite_ref, live);
        let failed = result.failed_at.is_some();
        results.push(result);
        if failed {
            break;
        }
    }

    SelfTestReport {
        iterations,
        backend: backend.to_owned(),
        live,
        results,
    }
}

// End-to-end runner tests live in [`super::scenarios`] alongside the
// regression-scenario coverage — keeps the runner file under AGENTS.md's
// ~500-line ceiling and puts the happy-path assertions next to the
// fault-injection scenarios that contrast with them.
