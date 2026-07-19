//! `whisper-dictate self-test injection-idempotency` — headless regression
//! test for INJECTION-side state accumulation bugs.
//!
//! Split across three siblings so no single file passes AGENTS.md's
//! ~500-line "new file" ceiling (Codex #518 review):
//!
//! * [`report`] — [`FailureStage`], [`IterationResult`], [`SelfTestReport`]
//!   plus the JSON / plain rendering. Runs on stock builds so the CLI
//!   contract stays exercised even without the feature flags.
//! * [`runner`] — the feature-gated [`run_iteration`] and the
//!   [`run_injection_idempotency_test`] entry point. Uses the same
//!   [`crate::hotkey::InjectionBracket`] the shipping
//!   `EnigoInjectBackend::inject` uses (Codex #518 F4).
//! * [`scenarios`] — regression tests that prove the harness ACTUALLY
//!   catches the two headline bug classes the module doc names —
//!   "modifier state leakage" and "unbalanced arm_end" — by inducing
//!   the fault at the primitive layer and asserting the detector fires
//!   (Codex + Claude #518 F1).
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
//!   watching). In the current architecture this manifests as the
//!   injection guard's bracket counter not returning to zero — the
//!   modifier release lives inside the RAII bracket, so a panic there
//!   leaves the bracket open. See
//!   [`scenarios::scenario_modifier_state_leak_is_detected_via_bracket_counter`].
//! * **Character position / queue state** — a per-string cursor, an
//!   internal typing buffer, or a chunker counter that carries over from
//!   burst N into burst N+1. Caught as `PlanDrift` in step 3.
//! * **Backend-selection cache going stale** — `pick_backend` on the
//!   same inputs must return the same backend across N calls, otherwise
//!   an env var that flipped between calls could silently switch the
//!   inject path mid-session. Codex #518 F8 extended step 3 to also
//!   compare each iteration's per-iter reference's backend/mode against
//!   the top-level reference, catching this even when the payload also
//!   shifts.
//! * **Guard bracket counter leaks** — every `arm_start` must be
//!   balanced by an `arm_end`; a leak would leave the hotkey listener
//!   permanently deaf to real user chords (a `ptt-wedge`-adjacent failure
//!   mode, caught here in the injection layer). See
//!   [`scenarios::scenario_unbalanced_arm_end_is_detected_by_active_brackets_check`].
//!
//! ## Approach — plan-level determinism AND direct state inspection,
//!    both on the default (dry-run) path
//!
//! Each dry-run iteration (the CI-safe default):
//!
//!   1. **Pre-snapshot** the shared state that could leak: the injection
//!      guard's `active_brackets` counter must be 0 and `is_active` must
//!      be false. A non-zero counter here is the concrete detection of
//!      the "unbalanced arm_end" bug class carrying over from a prior
//!      iteration (Codex #518 F2 — the state inspection now runs on the
//!      DEFAULT path, not just under `--live`).
//!   2. **Build the plan** for the current iteration's payload
//!      (`test text N`) via [`crate::injection::plan::build_plan`] — the
//!      exact code path `inject-text` uses.
//!   3. **Compare** the plan to a reference plan built once at the top of
//!      the run. Determinism failure = plan-building leaked state.
//!   4. **Round-trip** the plan through the production
//!      [`crate::hotkey::InjectionBracket`] RAII wrapper — the *same*
//!      primitive `EnigoInjectBackend::inject` uses (Codex #518 F4).
//!      Verify the counter is > 0 while the bracket is open.
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
//! ## Live paste iterations and clipboard state (Codex #518 F5)
//!
//! On `--live --backend paste`, the OS clipboard is a shared resource
//! that can leak content from iteration N-1 into iteration N — the
//! runner explicitly clears the clipboard between iterations when the
//! resolved mode is `paste`. On typing iterations the clipboard is
//! never touched. See [`runner::clear_clipboard_between_live_iterations`].
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

mod report;
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
mod runner;
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
mod scenarios;

pub use report::{FailureStage, IterationResult, SelfTestReport};

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub use runner::run_injection_idempotency_test;

/// Whether this build has the features needed to actually exercise the
/// idempotency assertions (both `rust-hotkeys` for the guard bracket
/// semantics AND `rust-injection` for the plan/execute surface). The CLI
/// handler consults this so a stock build prints a clear "rebuild"
/// message rather than reporting an empty pass.
pub const fn features_available() -> bool {
    cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
}

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
