//! Report shape for `self-test injection-idempotency`.
//!
//! Split out from the runner so the JSON/plain contract can be exercised
//! from a stock-feature build (no `rust-hotkeys` / `rust-injection`) — the
//! smoke script pins these fields and a rename must trip a compile error
//! here rather than a surprise at CLI-invocation time.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
