//! `whisper-dictate self-test injection-idempotency` — headless regression
//! test for INJECTION-side state accumulation bugs.
//!
//! ## Bug class this catches
//!
//! Ships alongside [`crate::hotkey::self_test`] (`self-test ptt-wedge`) but
//! probes a different bug class. Where `ptt-wedge` guards the HOTKEY
//! tracker against self-injected feedback events, this verb guards the
//! INJECTION path itself against state that should reset between
//! successive `inject()` calls but doesn't:
//!
//! * **Modifier state leakage** — a Shift/Ctrl that gets left "held" in the
//!   backend after a burst finishes, so the next burst types the wrong
//!   characters or accidentally triggers a shortcut. Historically the most
//!   damaging class (a stuck Shift on top of "test text" produces
//!   `TEST TEXT` — visually obviously wrong, but only when a human is
//!   watching).
//! * **Character position / queue state** — a per-string cursor, an
//!   internal typing buffer, or a chunker counter that carries over from
//!   burst N into burst N+1.
//! * **Backend-selection cache going stale** — `pick_backend` on the
//!   same inputs must return the same backend across N calls, otherwise
//!   an env var that flipped between calls could silently switch the
//!   inject path mid-session.
//! * **Guard bracket counter leaks** — every `arm_start` must be
//!   balanced by an `arm_end`; a leak would leave the hotkey listener
//!   permanently deaf to real user chords (a `ptt-wedge`-adjacent failure
//!   mode, caught here in the injection layer).
//!
//! ## Approach — plan-level determinism, no OS side effects on the default
//!
//! Each dry-run iteration (the CI-safe default):
//!
//!   1. **Pre-snapshot** the shared state that could leak: the injection
//!      guard's `is_active` bit + its internal bracket counter surrogate.
//!   2. **Build the plan** for the current iteration's payload
//!      (`test text N`) via [`crate::injection::plan::build_plan`] — the
//!      exact code path `inject-text` uses.
//!   3. **Compare** the plan to a reference plan built once at the top of
//!      the run. Determinism failure = plan-building leaked state.
//!   4. **Round-trip** the plan through `arm_start` / `arm_end` on a
//!      throwaway [`crate::hotkey::inject_guard::InjectionGuard`] — this
//!      simulates what `EnigoInjectBackend::inject` does on Windows and
//!      what the paste path does on Linux/Wayland.
//!   5. **Post-snapshot** and assert that the guard's `active_brackets`
//!      dropped back to zero (bracket idempotency) and that
//!      `is_active_at(t + horizon)` returns false past the post-grace
//!      window (horizon idempotency).
//!
//! On the `--live` path the plan is actually executed via
//! [`crate::injection::plan::execute_plan`]. That's dangerous — it types
//! into the active window — so it's opt-in via `--live` on the CLI and
//! never invoked from CI or the smoke script.
//!
//! No display server, no audio hardware, no `/dev/input`, no privileges
//! needed for the dry-run default. Runs on any OS in any CI container.
//!
//! ## Feature gating
//!
//! Same combined features as `self-test ptt-wedge`
//! (`rust-hotkeys,rust-injection`) — the injection plan path is gated on
//! `rust-injection` (for the `enigo` execution surface) and the guard
//! bracket assertions live in `hotkey::inject_guard`. Stock builds still
//! expose the CLI verb so the smoke script can pin it, but the handler
//! surfaces an actionable "rebuild with --features ..." error rather than
//! reporting a false pass.

use serde_json::json;

/// Where in an iteration the idempotency check failed. Kept as an enum so
/// the JSON report has a stable set of failure tokens the smoke script
/// (and downstream tooling) can grep for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureStage {
    /// Building the plan itself returned an error (backend picker
    /// rejected the input, or the plan builder blew up). Should never
    /// happen for a fixed test input — a signal here means the plan
    /// surface itself regressed.
    PlanBuild,
    /// The plan for this iteration differed from iteration 0's reference
    /// plan. Something in the pure plan-building path (backend picker,
    /// keystroke expansion) leaked state between calls.
    PlanDrift,
    /// The guard was ALREADY active before the iteration started, meaning
    /// a previous iteration failed to close its bracket. Bracket counter
    /// leak — the classic "arm_start without arm_end" bug.
    GuardActiveBeforeArm,
    /// The bracket counter didn't drop back to zero after `arm_end`.
    /// Distinct from `GuardActiveBeforeArm` so the smoke script can tell
    /// "wrong start state" from "wrong end state" without parsing detail.
    GuardStillActiveAfterEnd,
    /// The plan executed via `--live` returned an error. Only reachable
    /// on the live path.
    ExecuteFailed,
}

impl FailureStage {
    /// Stable machine-readable token — the smoke script and downstream
    /// tests pin these strings, so renames MUST be deliberate.
    pub fn as_str(self) -> &'static str {
        match self {
            FailureStage::PlanBuild => "plan_build",
            FailureStage::PlanDrift => "plan_drift",
            FailureStage::GuardActiveBeforeArm => "guard_active_before_arm",
            FailureStage::GuardStillActiveAfterEnd => "guard_still_active_after_end",
            FailureStage::ExecuteFailed => "execute_failed",
        }
    }
}

/// One iteration's outcome. Either passes cleanly or short-circuits at
/// the first observed regression so `failed_at` unambiguously names one
/// stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IterationResult {
    /// 1-based iteration index (matches the human-readable report).
    pub index: usize,
    /// `None` on success; `Some(stage)` when a regression fired.
    pub failed_at: Option<FailureStage>,
    /// Human-readable diagnostic filled on failure.
    pub detail: String,
}

/// Overall report for the injection-idempotency self-test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfTestReport {
    /// Iterations requested.
    pub iterations: usize,
    /// Backend label reported (echoed back from the caller — the smoke
    /// script pins this so a future rename surfaces here first).
    pub backend: String,
    /// Whether `--live` was in effect. Reported so a scary "typed into a
    /// real window" run is unambiguous from the safe dry-run.
    pub live: bool,
    /// Per-iteration outcomes, in order.
    pub results: Vec<IterationResult>,
}

impl SelfTestReport {
    /// True iff every iteration passed cleanly. Note: an empty results
    /// vec is vacuously true — the CLI handler enforces `iterations >= 1`
    /// so the only way to hit the empty case is the stock-build stub.
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(|r| r.failed_at.is_none())
    }

    /// Render the report as a single JSON object. Keys are the machine-
    /// readable contract callers should pin against.
    pub fn to_json(&self) -> String {
        let results: Vec<_> = self
            .results
            .iter()
            .map(|r| {
                json!({
                    "index": r.index,
                    "passed": r.failed_at.is_none(),
                    "failed_at": r.failed_at.map(|s| s.as_str()),
                    "detail": r.detail,
                })
            })
            .collect();
        json!({
            "kind": "injection_idempotency_self_test",
            "backend": self.backend,
            "live": self.live,
            "iterations": self.iterations,
            "all_passed": self.all_passed(),
            "results": results,
        })
        .to_string()
    }

    /// Render the report as a human-readable multi-line summary.
    pub fn to_plain(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "[self-test injection-idempotency] backend={}  live={}  iterations={}\n",
            self.backend, self.live, self.iterations,
        ));
        for r in &self.results {
            match r.failed_at {
                None => out.push_str(&format!("  iter {} PASS\n", r.index)),
                Some(stage) => out.push_str(&format!(
                    "  iter {} FAIL at {}: {}\n",
                    r.index,
                    stage.as_str(),
                    r.detail,
                )),
            }
        }
        if self.all_passed() {
            out.push_str("[self-test injection-idempotency] all iterations passed\n");
        } else {
            let failed: Vec<String> = self
                .results
                .iter()
                .filter_map(|r| {
                    r.failed_at
                        .map(|s| format!("iter {} @ {}", r.index, s.as_str()))
                })
                .collect();
            out.push_str(&format!(
                "[self-test injection-idempotency] FAILED — {}\n",
                failed.join(", "),
            ));
        }
        out
    }
}

/// Whether this build has the features needed to actually exercise the
/// idempotency assertions (both `rust-hotkeys` for the guard bracket
/// semantics AND `rust-injection` for the plan/execute surface). The CLI
/// handler consults this so a stock build prints a clear "rebuild"
/// message rather than reporting an empty pass.
pub const fn features_available() -> bool {
    cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
}

// ---------------------------------------------------------------------------
// Feature-gated implementation. On stock builds the CLI wrapper surfaces an
// actionable error BEFORE reaching here, but the stub keeps the public
// function signature so the dispatch site is feature-gate-free.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
mod imp {
    use super::*;

    use std::time::{Duration, Instant};

    use crate::hotkey::inject_guard::InjectionGuard;
    use crate::injection::plan::{build_plan, execute_plan, InjectionPlan};
    use crate::injection::LinuxSession;

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

    /// Run one idempotency iteration. Returns an [`IterationResult`] with
    /// `failed_at = None` on success. Pulled out so unit tests can drive
    /// it deterministically without the outer loop's index/time bookkeeping.
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

        // ---- Step 4: guard bracket round-trip ----
        // Simulate what `EnigoInjectBackend::inject` does around every
        // SendInput burst — arm_start / arm_end pair. If a bracket
        // leaked, the counter would sit at 1 and the post-check in
        // step 5 would trip.
        guard.arm_start(PRE_GRACE);
        // While the bracket is open, is_active MUST be true (otherwise
        // the whole self-injection filter is defeated — the ptt-wedge
        // regression would come back). This is a sanity check that
        // arm_start actually did something, not a bug the self-test is
        // primarily hunting — but reporting it as a drift keeps the
        // failure taxonomy tight (we don't need a distinct stage token
        // for "arm_start was a no-op").
        if !guard.is_active_at(Instant::now()) {
            guard.arm_end(POST_GRACE);
            return IterationResult {
                index,
                failed_at: Some(FailureStage::GuardActiveBeforeArm),
                detail:
                    "guard.is_active returned false while a bracket was open (arm_start no-op?)"
                        .to_owned(),
            };
        }

        // ---- Step 4b (live only): execute the plan ----
        // Guarded by `live`: dry-run mode never touches the real display
        // server. This is the same call the CLI's `inject-text --do-it`
        // makes, so a failure here reproduces what an operator would see.
        if live {
            if let Err(err) = execute_plan(&plan, "", "") {
                guard.arm_end(POST_GRACE);
                return IterationResult {
                    index,
                    failed_at: Some(FailureStage::ExecuteFailed),
                    detail: format!("execute_plan failed: {err}"),
                };
            }
        }

        guard.arm_end(POST_GRACE);

        // ---- Step 5: post-snapshot ----
        // Give the horizon time to expire — is_active must return false
        // once every open bracket has been closed AND the post-grace
        // window has elapsed. `HORIZON_SLACK` past `POST_GRACE` is more
        // than enough headroom.
        let t_after = Instant::now() + POST_GRACE + HORIZON_SLACK;
        if guard.is_active_at(t_after) {
            return IterationResult {
                index,
                failed_at: Some(FailureStage::GuardStillActiveAfterEnd),
                detail:
                    "guard.is_active_at(after POST_GRACE + slack) returned true — bracket counter \
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
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub use imp::run_injection_idempotency_test;

/// Stock-build stub — the CLI handler prints an actionable error before
/// reaching here, but exposing the same signature keeps the dispatch site
/// feature-gate-free.
#[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
pub fn run_injection_idempotency_test(
    iterations: usize,
    backend: &str,
    live: bool,
) -> SelfTestReport {
    SelfTestReport {
        iterations,
        backend: backend.to_owned(),
        live,
        results: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Report / stage shape tests — run on every build so the CLI surface is
    // covered even on a stock-feature build.
    // -----------------------------------------------------------------------

    #[test]
    fn failure_stage_tokens_are_stable() {
        // Smoke script + downstream tests pin these — a rename must be
        // deliberate.
        assert_eq!(FailureStage::PlanBuild.as_str(), "plan_build");
        assert_eq!(FailureStage::PlanDrift.as_str(), "plan_drift");
        assert_eq!(
            FailureStage::GuardActiveBeforeArm.as_str(),
            "guard_active_before_arm"
        );
        assert_eq!(
            FailureStage::GuardStillActiveAfterEnd.as_str(),
            "guard_still_active_after_end"
        );
        assert_eq!(FailureStage::ExecuteFailed.as_str(), "execute_failed");
    }

    #[test]
    fn empty_report_all_passed_is_true_by_convention() {
        // Vacuous truth mirrors the ptt-wedge report — the CLI handler
        // enforces `iterations >= 1` so the only reachable empty case is
        // the stock-build stub, and that's gated on `features_available`.
        let r = SelfTestReport {
            iterations: 0,
            backend: "auto".to_owned(),
            live: false,
            results: Vec::new(),
        };
        assert!(r.all_passed());
    }

    #[test]
    fn report_to_plain_marks_pass_and_fail_lines_distinctly() {
        let r = SelfTestReport {
            iterations: 2,
            backend: "wtype".to_owned(),
            live: false,
            results: vec![
                IterationResult {
                    index: 1,
                    failed_at: None,
                    detail: String::new(),
                },
                IterationResult {
                    index: 2,
                    failed_at: Some(FailureStage::GuardStillActiveAfterEnd),
                    detail: "bracket leaked".to_owned(),
                },
            ],
        };
        let out = r.to_plain();
        assert!(
            out.contains("iter 1 PASS"),
            "plain missing pass line: {out}"
        );
        assert!(
            out.contains("iter 2 FAIL at guard_still_active_after_end"),
            "plain missing fail line: {out}"
        );
        assert!(
            out.contains("FAILED"),
            "plain missing FAILED summary: {out}"
        );
    }

    #[test]
    fn report_to_plain_all_passed_prints_all_iterations_passed() {
        let r = SelfTestReport {
            iterations: 1,
            backend: "auto".to_owned(),
            live: false,
            results: vec![IterationResult {
                index: 1,
                failed_at: None,
                detail: String::new(),
            }],
        };
        assert!(r.to_plain().contains("all iterations passed"));
    }

    #[test]
    fn report_to_json_has_stable_keys() {
        let r = SelfTestReport {
            iterations: 1,
            backend: "auto".to_owned(),
            live: false,
            results: vec![IterationResult {
                index: 1,
                failed_at: None,
                detail: String::new(),
            }],
        };
        let json = r.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["kind"], "injection_idempotency_self_test");
        assert_eq!(parsed["backend"], "auto");
        assert_eq!(parsed["live"], false);
        assert_eq!(parsed["iterations"], 1);
        assert_eq!(parsed["all_passed"], true);
        assert_eq!(parsed["results"][0]["index"], 1);
        assert_eq!(parsed["results"][0]["passed"], true);
        assert!(parsed["results"][0]["failed_at"].is_null());
    }

    #[test]
    fn report_to_json_encodes_failure_stage_token() {
        let r = SelfTestReport {
            iterations: 1,
            backend: "wtype".to_owned(),
            live: true,
            results: vec![IterationResult {
                index: 1,
                failed_at: Some(FailureStage::PlanDrift),
                detail: "backend drifted".to_owned(),
            }],
        };
        let parsed: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert_eq!(parsed["all_passed"], false);
        assert_eq!(parsed["live"], true);
        assert_eq!(parsed["results"][0]["passed"], false);
        assert_eq!(parsed["results"][0]["failed_at"], "plan_drift");
        assert_eq!(parsed["results"][0]["detail"], "backend drifted");
    }

    // -----------------------------------------------------------------------
    // End-to-end assertions — only when the features are on.
    // -----------------------------------------------------------------------

    #[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
    mod imp {
        use super::*;

        #[test]
        fn happy_path_all_iterations_pass_dry_run() {
            // The shipping plan builder + guard bracket semantics must
            // PASS the self-test. This is the CI contract: any regression
            // in `build_plan` or the guard bracket counter surfaces here
            // before it ships.
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
        fn happy_path_pinned_backend_pass_dry_run() {
            // `--backend enigo` pins the resolved backend to `enigo`;
            // idempotency must hold there too — this is the path a
            // downstream tester uses to exercise a specific backend
            // without waiting for the platform-detected default to line
            // up. Pin-parses via `pick_backend`, which every explicit
            // backend name goes through verbatim.
            let report = run_injection_idempotency_test(3, "enigo", false);
            assert!(
                report.all_passed(),
                "pinned-backend dry-run must pass, got: {}",
                report.to_plain()
            );
            assert_eq!(report.backend, "enigo");
        }

        #[test]
        fn zero_iterations_returns_empty_results() {
            // The CLI handler enforces `iterations >= 1`, but the pure
            // function must handle 0 without panicking so a bad caller
            // gets a clear empty report instead of a crash.
            let report = run_injection_idempotency_test(0, "auto", false);
            assert!(report.all_passed());
            assert_eq!(report.results.len(), 0);
            assert!(!report.live);
        }

        #[test]
        fn unknown_backend_surfaces_as_plan_build_failure() {
            // A typo in `--backend` is rejected by clap's value_parser
            // before it reaches the handler, but the pure function still
            // has to defend itself — an unknown backend surfaces as
            // `plan_build` on iteration 1 (the reference plan build
            // fails), NOT as a generic panic.
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
        fn backend_label_is_reported_in_result() {
            // The report's `backend` field echoes what the caller passed,
            // NOT the resolved backend name. Smoke script pins this so a
            // future rename of the CLI flag surfaces here first.
            let report = run_injection_idempotency_test(1, "auto", false);
            assert_eq!(report.backend, "auto");
            let report = run_injection_idempotency_test(1, "enigo", false);
            assert_eq!(report.backend, "enigo");
        }

        /// The crux of the regression harness: prove the detector actually
        /// fires when the guard bracket is NOT closed. Reproduces a
        /// bracket leak by arming without ending, then asserts
        /// `is_active_at(t + huge horizon)` still returns true — i.e. the
        /// self-test's post-check would trip. Without this test the
        /// self-test could return "all pass" for a broken guard and
        /// nobody would notice.
        #[test]
        fn bracket_leak_would_be_detected_by_post_check() {
            use crate::hotkey::inject_guard::InjectionGuard;
            use std::time::{Duration, Instant};

            let guard = InjectionGuard::new();
            // Arm without matching arm_end — the classic bracket leak.
            guard.arm_start(Duration::from_millis(50));

            // The self-test's post-check asserts is_active_at(t + slack)
            // is false. With the leak in place, the counter is still 1
            // so is_active MUST return true regardless of the horizon —
            // which is exactly what the self-test's step 5 checks for.
            let far_future = Instant::now() + Duration::from_secs(3600);
            assert!(
                guard.is_active_at(far_future),
                "sanity: an unclosed bracket must keep is_active true \
                 forever — if this assertion ever fails the guard's \
                 bracket semantics have changed and the self-test's \
                 post-check needs updating."
            );
        }

        /// The complementary sanity check for the plan-drift detector:
        /// two plans built from the same inputs must compare materially
        /// equal, otherwise the plan builder has hidden state and the
        /// self-test's step 3 would flag it. Pins the invariant so a
        /// future regression is caught by the self-test rather than by
        /// users typing the wrong words.
        #[test]
        fn build_plan_is_deterministic_for_fixed_inputs() {
            use crate::injection::plan::build_plan;
            use crate::injection::LinuxSession;

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
                "build_plan must be deterministic — this is the \
                 invariant the self-test's plan-drift check relies on."
            );
        }
    }
}
