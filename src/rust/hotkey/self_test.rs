//! `whisper-dictate self-test ptt-wedge` — headless regression test for the
//! self-injection PTT-wedge class of bugs that broke v1.20.7 (Windows) and
//! v1.20.2 (Wayland via #467).
//!
//! ## What this catches
//!
//! On Windows the enigo injector's `SendInput` bursts feed back into rdev's
//! `WH_KEYBOARD_LL` hook. Every injected foreign key (unmapped VKs, plus the
//! `STALE_MODIFIER_VKS` release sweep) can populate the tracker's `pressed`
//! map, tripping bare-modifier rule 1 for the NEXT PTT press — the user's
//! next chord silently fails to fire until the 10 s foreign-key self-heal
//! expires. See [`crate::hotkey::inject_guard`] for the full timing model
//! and [`docs/design/item5-wire-dictate-session.md`] for the design context.
//!
//! v1.20.7 shipped without a matching regression test — this module is that
//! test, exercised both by unit tests here and by a CLI verb the smoke
//! script runs in CI.
//!
//! ## Approach — pure code paths, no OS hooks
//!
//! Every iteration:
//!
//!   1. Simulates the FIRST PTT chord (Ctrl_L press → Shift_L press) through
//!      the same [`crate::hotkey::inject_guard::dispatch_raw_event`] entry
//!      point the rdev/evdev drivers call on every OS key event. Confirms the
//!      tracker emits `ChordPress`.
//!   2. Simulates the release of that chord — confirms `ChordRelease`.
//!   3. Simulates a transcription's injection burst: opens an
//!      [`crate::hotkey::InjectionGuard`] bracket, and while the bracket is
//!      open, feeds synthetic self-injected events (an unmapped VK plus a
//!      `STALE_MODIFIER_VKS`-shaped `ctrl_r` release) through the same
//!      dispatch — they MUST NOT reach the tracker.
//!   4. Closes the bracket, then simulates the user's SECOND PTT chord —
//!      MUST fire `ChordPress` again. Without the guard, step 3's foreign
//!      press would sit in the tracker's `pressed` map and rule 1 would
//!      block step 4's chord. Iteration fails if `ChordPress` doesn't fire.
//!
//! No `rdev::listen`, no `/dev/input`, no display server, no privileges —
//! runs on any OS in any CI container. The exact classes of feedback events
//! we simulate (unmapped-VK synthetic name, `STALE_MODIFIER_VKS` VK release)
//! are precisely what the v1.20.7 wedge saw in production.
//!
//! ## Feature gating
//!
//! The self-test needs both the tracker + guard and the injector's
//! `arm_start`/`arm_end` semantics, so it's gated on
//! `rust-hotkeys,rust-injection` — the same combined features Phase A of item
//! 5 requires. Stock builds still expose the CLI verb but exit with an
//! actionable "rebuild with --features …" message so the wayland-user-smoke
//! script can pin the check without a feature-gate at the shell level.

use serde_json::json;

/// Which listener path the test claims to exercise. The self-test drives the
/// tracker + guard directly (both drivers converge on the same
/// [`crate::hotkey::inject_guard::dispatch_raw_event`] filter), so `--driver`
/// only affects the reported label — the underlying assertions are identical.
///
/// Kept as an explicit enum (rather than passing the raw string through) so
/// the CLI can reject typos BEFORE running the test loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfTestDriver {
    /// Auto — same label the CLI's default emits.
    Auto,
    /// rdev listener path (Windows / macOS / Linux X11).
    Rdev,
    /// evdev listener path (Linux / Wayland).
    Evdev,
}

impl SelfTestDriver {
    /// Parse a `--driver` value into a [`SelfTestDriver`]. Accepts the same
    /// canonical names and aliases (`x11` / `wayland`) that the manager's
    /// `DriverKind::parse` recognises so the two flag surfaces agree.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "rdev" | "x11" => Some(Self::Rdev),
            "evdev" | "wayland" => Some(Self::Evdev),
            _ => None,
        }
    }

    /// Stable label for the report — pinned so callers (smoke scripts) can
    /// grep it.
    pub fn label(self) -> &'static str {
        match self {
            SelfTestDriver::Auto => "auto",
            SelfTestDriver::Rdev => "rdev",
            SelfTestDriver::Evdev => "evdev",
        }
    }
}

/// Where in an iteration the wedge was detected. Kept as an enum (rather
/// than a free-form string) so the JSON report has a fixed set of failure
/// tokens tests can pin against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WedgeStage {
    /// The FIRST PTT chord (before any injection) failed to fire — the
    /// tracker itself is broken, not the guard. Extremely unlikely but
    /// reported distinctly so a false-positive doesn't get blamed on the
    /// wedge.
    FirstChordPress,
    /// The first chord's release failed to fire (tracker didn't drop the
    /// held modifiers).
    FirstChordRelease,
    /// An injected event LEAKED through the guard and reached the tracker
    /// (something returned `Some(...)` from `dispatch_raw_event` while the
    /// bracket was open). This is the classic v1.20.7 symptom.
    InjectedEventLeaked,
    /// The SECOND PTT chord (after the injection burst) failed to fire —
    /// the wedge is present. This is the primary regression signal.
    SecondChordPress,
}

impl WedgeStage {
    fn as_str(self) -> &'static str {
        match self {
            WedgeStage::FirstChordPress => "first_chord_press",
            WedgeStage::FirstChordRelease => "first_chord_release",
            WedgeStage::InjectedEventLeaked => "injected_event_leaked",
            WedgeStage::SecondChordPress => "second_chord_press",
        }
    }
}

/// One iteration's outcome. Every iteration either passes cleanly or fails
/// at exactly one stage — we short-circuit the iteration on the first
/// failure so the reported stage is unambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IterationResult {
    /// 1-based iteration index (matches the human-readable report).
    pub index: usize,
    /// `None` on success; `Some(stage)` when the wedge signal fired.
    pub failed_at: Option<WedgeStage>,
    /// Human-readable diagnostic — populated on failure with the extra
    /// context the enum can't carry (e.g. which foreign key leaked).
    pub detail: String,
}

/// Overall self-test report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfTestReport {
    /// Iterations requested.
    pub iterations: usize,
    /// Driver label reported to the caller.
    pub driver: &'static str,
    /// Per-iteration outcomes, in order.
    pub results: Vec<IterationResult>,
}

impl SelfTestReport {
    /// True when every iteration passed.
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(|r| r.failed_at.is_none())
    }

    /// Render the report as one JSON object per line's worth of information
    /// — a single JSON object with `iterations`, `driver`, `all_passed`, and
    /// a `results` array. Keys are the machine-readable contract.
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
            "kind": "ptt_wedge_self_test",
            "driver": self.driver,
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
            "[self-test ptt-wedge] driver={}  iterations={}\n",
            self.driver, self.iterations
        ));
        for r in &self.results {
            match r.failed_at {
                None => out.push_str(&format!("  iter {} PASS\n", r.index)),
                Some(stage) => out.push_str(&format!(
                    "  iter {} FAIL at {}: {}\n",
                    r.index,
                    stage.as_str(),
                    r.detail
                )),
            }
        }
        if self.all_passed() {
            out.push_str("[self-test ptt-wedge] all iterations passed\n");
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
                "[self-test ptt-wedge] FAILED — {}\n",
                failed.join(", ")
            ));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Feature-gated implementation. When `rust-hotkeys` + `rust-injection` are
// compiled in we drive the real guard + tracker. On stock builds the CLI
// entry point returns an actionable error rather than compiling a stub loop
// that could mask a regression.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
mod imp {
    use super::*;

    use std::time::{Duration, Instant};

    use crate::hotkey::inject_guard::{dispatch_raw_event, InjectionGuard};
    use crate::hotkey::manager::tracker::{KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput};

    /// The PTT chord the self-test uses. Ctrl_L + Shift_L is a canonical bare-
    /// modifier binding — the class that trips bare-modifier rule 1 (the
    /// wedge's primary vector). Kept as a constant so the regression test's
    /// input doesn't drift with settings changes.
    const TEST_CHORD: [&str; 2] = ["ctrl_l", "shift_l"];

    /// Foreign keys the "injection burst" pretends to synthesise. Mirrors
    /// the real classes of feedback the v1.20.7 wedge saw:
    ///
    /// * `__rdev_Unknown(231)` — an unmapped VK enigo's `text` bursts
    ///   through the LL-hook (rdev returns the synthetic `__rdev_` name).
    /// * `ctrl_r` release — a member of
    ///   [`crate::dictate::backends::inject::STALE_MODIFIER_VKS`] which rdev
    ///   DOES resolve to a real name; without the guard this drops the
    ///   held-ctrl_l tracker entry AND leaves ctrl_r flagged as a foreign
    ///   key press for the next 10 s.
    /// * `__rdev_Unknown(97)` — a second unmapped VK to exercise the guard
    ///   during a multi-event burst (not just a single press).
    ///
    /// The exact names don't matter for the pass/fail signal — what matters
    /// is that `dispatch_raw_event` drops every one of them while the
    /// bracket is open.
    fn injected_press(name: &str, at: Instant) -> RawKeyEvent {
        RawKeyEvent {
            name: name.to_owned(),
            kind: RawKeyKind::Press,
            at,
        }
    }
    fn injected_release(name: &str, at: Instant) -> RawKeyEvent {
        RawKeyEvent {
            name: name.to_owned(),
            kind: RawKeyKind::Release,
            at,
        }
    }

    /// Run the self-test once. Pulled out so unit tests can drive the same
    /// path deterministically without going through the outer loop's
    /// timing.
    pub(super) fn run_iteration(index: usize, base: Instant) -> IterationResult {
        let guard = InjectionGuard::new();
        let mut tracker = KeyTracker::new(TEST_CHORD.iter().map(|s| (*s).to_owned()).collect());

        // Step 1: user's FIRST PTT chord press.
        let t0 = base;
        let ev_ctrl = injected_press("ctrl_l", t0);
        // First modifier of a two-key bare-modifier chord — no output yet.
        if dispatch_raw_event(&guard, &mut tracker, &ev_ctrl).is_some() {
            return IterationResult {
                index,
                failed_at: Some(WedgeStage::FirstChordPress),
                detail:
                    "first ctrl_l press unexpectedly produced tracker output before chord completed"
                        .to_owned(),
            };
        }
        let ev_shift = injected_press("shift_l", t0 + Duration::from_millis(5));
        match dispatch_raw_event(&guard, &mut tracker, &ev_shift) {
            Some(TrackerOutput::ChordPress) => {}
            other => {
                return IterationResult {
                    index,
                    failed_at: Some(WedgeStage::FirstChordPress),
                    detail: format!(
                        "expected ChordPress on shift_l press completing the chord, got {other:?}"
                    ),
                };
            }
        }

        // Step 2: user releases the chord.
        let ev_shift_up = injected_release("shift_l", t0 + Duration::from_millis(200));
        match dispatch_raw_event(&guard, &mut tracker, &ev_shift_up) {
            Some(TrackerOutput::ChordRelease) => {}
            other => {
                return IterationResult {
                    index,
                    failed_at: Some(WedgeStage::FirstChordRelease),
                    detail: format!("expected ChordRelease on shift_l release, got {other:?}"),
                };
            }
        }
        let ev_ctrl_up = injected_release("ctrl_l", t0 + Duration::from_millis(210));
        if dispatch_raw_event(&guard, &mut tracker, &ev_ctrl_up).is_some() {
            return IterationResult {
                index,
                failed_at: Some(WedgeStage::FirstChordRelease),
                detail: "trailing ctrl_l release unexpectedly produced tracker output".to_owned(),
            };
        }

        // Step 3: transcription's injection burst. Bracket the guard exactly
        // as `EnigoInjectBackend::inject` does. `arm_start` grace mirrors
        // `INJECT_PRE_GRACE` in the shipping injector; the exact value is
        // not load-bearing here because the counter alone keeps the guard
        // active for the whole synthetic burst.
        let t_inject = t0 + Duration::from_millis(500);
        guard.arm_start(Duration::from_millis(50));

        // Feed a realistic burst of foreign events. Each one MUST be dropped
        // by dispatch_raw_event — a leak indicates the guard is not covering
        // the burst window.
        let burst = [
            injected_press("__rdev_Unknown(231)", t_inject + Duration::from_millis(1)),
            injected_release("__rdev_Unknown(231)", t_inject + Duration::from_millis(2)),
            // STALE_MODIFIER_VKS release-sweep-shaped event: a real rdev-
            // resolvable modifier name. Without the guard this simultaneously
            // (a) drops the tracker's ctrl_l entry via family-match, and
            // (b) sits as an accumulated foreign key that the next PTT
            // press would trip rule 1 against.
            injected_release("ctrl_r", t_inject + Duration::from_millis(3)),
            injected_press("__rdev_Unknown(97)", t_inject + Duration::from_millis(4)),
            injected_release("__rdev_Unknown(97)", t_inject + Duration::from_millis(5)),
        ];
        for event in &burst {
            if let Some(out) = dispatch_raw_event(&guard, &mut tracker, event) {
                guard.arm_end(Duration::from_millis(200));
                return IterationResult {
                    index,
                    failed_at: Some(WedgeStage::InjectedEventLeaked),
                    detail: format!(
                        "injected event {name:?} ({kind:?}) leaked past guard as {out:?}",
                        name = event.name,
                        kind = event.kind
                    ),
                };
            }
        }
        guard.arm_end(Duration::from_millis(200));

        // Step 4: user re-presses PTT AFTER the injection window. The whole
        // point of the test — this MUST fire ChordPress. Without the guard,
        // the ctrl_r release in step 3 would have left ctrl_r pressed OR
        // dropped ctrl_l (either way tripping the next chord), and the
        // unmapped-VK presses would sit as held foreign keys blocking rule
        // 1. The second chord uses timestamps well past the guard's
        // post-grace horizon so we're testing the tracker's state, not the
        // guard's residual filtering.
        let t_next = t_inject + Duration::from_millis(500);
        let ev_ctrl2 = injected_press("ctrl_l", t_next);
        if dispatch_raw_event(&guard, &mut tracker, &ev_ctrl2).is_some() {
            return IterationResult {
                index,
                failed_at: Some(WedgeStage::SecondChordPress),
                detail:
                    "second ctrl_l press unexpectedly produced tracker output before chord completed \
                     — indicates stale tracker state from unfiltered injection burst"
                        .to_owned(),
            };
        }
        let ev_shift2 = injected_press("shift_l", t_next + Duration::from_millis(5));
        match dispatch_raw_event(&guard, &mut tracker, &ev_shift2) {
            Some(TrackerOutput::ChordPress) => IterationResult {
                index,
                failed_at: None,
                detail: String::new(),
            },
            other => IterationResult {
                index,
                failed_at: Some(WedgeStage::SecondChordPress),
                detail: format!(
                    "expected ChordPress on second chord after injection, got {other:?} \
                     — this is the v1.20.7 Windows PTT wedge signal"
                ),
            },
        }
    }

    /// Public entry — spins `iterations` runs of [`run_iteration`], stopping
    /// at the first failure. Each iteration is independent (own guard, own
    /// tracker) so a failure of one doesn't cascade into the next.
    pub fn run_ptt_wedge_test(iterations: usize, driver: SelfTestDriver) -> SelfTestReport {
        let base = Instant::now();
        let mut results = Vec::with_capacity(iterations);
        for i in 1..=iterations {
            // Space iterations out on the mock clock so if the tracker's
            // foreign-key expiry ever comes into play it doesn't confound
            // one iteration's state with another's. In practice each
            // iteration constructs a fresh tracker so this is defensive.
            let iter_base = base + Duration::from_secs(u64::try_from(i).unwrap_or(1) * 30);
            let result = run_iteration(i, iter_base);
            let failed = result.failed_at.is_some();
            results.push(result);
            if failed {
                break;
            }
        }
        SelfTestReport {
            iterations,
            driver: driver.label(),
            results,
        }
    }
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub use imp::run_ptt_wedge_test;

/// Stock-build stub — the CLI wrapper prints an actionable error before
/// reaching here, but exposing the same function signature lets the CLI
/// handler keep a single call shape.
#[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
pub fn run_ptt_wedge_test(iterations: usize, driver: SelfTestDriver) -> SelfTestReport {
    SelfTestReport {
        iterations,
        driver: driver.label(),
        results: Vec::new(),
    }
}

/// Whether this build has the features needed to actually exercise the
/// wedge (both `rust-hotkeys` for the tracker + guard AND `rust-injection`
/// for the injector's arm semantics). Consulted by the CLI handler to
/// print a clear "rebuild" message rather than reporting an empty pass.
pub const fn features_available() -> bool {
    cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Driver / stage / report shape tests — run on every build so the CLI
    // surface is covered even on stock builds.
    // -----------------------------------------------------------------------

    #[test]
    fn driver_parse_accepts_canonical_names() {
        assert_eq!(SelfTestDriver::parse("auto"), Some(SelfTestDriver::Auto));
        assert_eq!(SelfTestDriver::parse("rdev"), Some(SelfTestDriver::Rdev));
        assert_eq!(SelfTestDriver::parse("evdev"), Some(SelfTestDriver::Evdev));
    }

    #[test]
    fn driver_parse_accepts_x11_and_wayland_aliases() {
        assert_eq!(SelfTestDriver::parse("x11"), Some(SelfTestDriver::Rdev));
        assert_eq!(
            SelfTestDriver::parse("wayland"),
            Some(SelfTestDriver::Evdev)
        );
    }

    #[test]
    fn driver_parse_is_case_insensitive_and_trims() {
        assert_eq!(
            SelfTestDriver::parse(" EVDEV "),
            Some(SelfTestDriver::Evdev)
        );
        assert_eq!(SelfTestDriver::parse("Rdev"), Some(SelfTestDriver::Rdev));
    }

    #[test]
    fn driver_parse_empty_is_auto() {
        assert_eq!(SelfTestDriver::parse(""), Some(SelfTestDriver::Auto));
    }

    #[test]
    fn driver_parse_unknown_returns_none() {
        assert_eq!(SelfTestDriver::parse("uinput"), None);
        assert_eq!(SelfTestDriver::parse("garbage"), None);
    }

    #[test]
    fn driver_label_is_stable() {
        assert_eq!(SelfTestDriver::Auto.label(), "auto");
        assert_eq!(SelfTestDriver::Rdev.label(), "rdev");
        assert_eq!(SelfTestDriver::Evdev.label(), "evdev");
    }

    #[test]
    fn wedge_stage_str_tokens_are_stable() {
        // Smoke script pins these — a rename must be a deliberate change.
        assert_eq!(WedgeStage::FirstChordPress.as_str(), "first_chord_press");
        assert_eq!(
            WedgeStage::FirstChordRelease.as_str(),
            "first_chord_release"
        );
        assert_eq!(
            WedgeStage::InjectedEventLeaked.as_str(),
            "injected_event_leaked"
        );
        assert_eq!(WedgeStage::SecondChordPress.as_str(), "second_chord_press");
    }

    #[test]
    fn empty_report_all_passed_is_true_by_convention() {
        // Vacuous truth: an empty results vec means "no failures observed".
        // The CLI handler enforces `iterations >= 1` before calling in, so
        // the empty case is only reachable via the stock-build stub — where
        // exit 0 would be wrong. The CLI guards that with an explicit
        // features check; this test just pins the report semantics.
        let r = SelfTestReport {
            iterations: 0,
            driver: "auto",
            results: Vec::new(),
        };
        assert!(r.all_passed());
    }

    #[test]
    fn report_to_plain_marks_pass_and_fail_lines_distinctly() {
        let r = SelfTestReport {
            iterations: 2,
            driver: "rdev",
            results: vec![
                IterationResult {
                    index: 1,
                    failed_at: None,
                    detail: String::new(),
                },
                IterationResult {
                    index: 2,
                    failed_at: Some(WedgeStage::SecondChordPress),
                    detail: "wedge fired".to_owned(),
                },
            ],
        };
        let out = r.to_plain();
        assert!(
            out.contains("iter 1 PASS"),
            "plain missing pass line: {out}"
        );
        assert!(
            out.contains("iter 2 FAIL at second_chord_press"),
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
            driver: "auto",
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
            driver: "evdev",
            results: vec![IterationResult {
                index: 1,
                failed_at: None,
                detail: String::new(),
            }],
        };
        let json = r.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["kind"], "ptt_wedge_self_test");
        assert_eq!(parsed["driver"], "evdev");
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
            driver: "rdev",
            results: vec![IterationResult {
                index: 1,
                failed_at: Some(WedgeStage::InjectedEventLeaked),
                detail: "injected event leaked".to_owned(),
            }],
        };
        let parsed: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert_eq!(parsed["all_passed"], false);
        assert_eq!(parsed["results"][0]["passed"], false);
        assert_eq!(parsed["results"][0]["failed_at"], "injected_event_leaked");
        assert_eq!(parsed["results"][0]["detail"], "injected event leaked");
    }

    // -----------------------------------------------------------------------
    // End-to-end wedge-detection assertions — only when the features are on.
    // -----------------------------------------------------------------------

    #[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
    mod imp {
        use super::*;

        #[test]
        fn happy_path_all_iterations_pass_with_guard() {
            // The shipping guard bracket + tracker filter must PASS the
            // self-test — this is the CI contract that would have caught
            // v1.20.7 before it shipped.
            let report = run_ptt_wedge_test(5, SelfTestDriver::Auto);
            assert!(
                report.all_passed(),
                "expected all iterations to pass, got: {}",
                report.to_plain()
            );
            assert_eq!(report.iterations, 5);
            assert_eq!(report.results.len(), 5);
            assert_eq!(report.driver, "auto");
        }

        /// Negative test — the crux of the regression harness. Prove the
        /// wedge detection actually fires when the guard is NOT bracketing
        /// the burst. Reproduces what v1.20.7 did (feed the injected events
        /// straight into the tracker) and asserts the tracker's second chord
        /// is blocked — i.e. our detector triggers. Without this test the
        /// self-test could return "all pass" for a broken subject-under-test
        /// and nobody would notice.
        #[test]
        fn without_guard_bracketing_wedge_is_detected() {
            use crate::hotkey::inject_guard::{dispatch_raw_event, InjectionGuard};
            use crate::hotkey::manager::tracker::{
                KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput,
            };
            use std::time::{Duration, Instant};

            let guard = InjectionGuard::new(); // NEVER armed — simulates
                                               // the pre-#507 world.
            let mut tracker = KeyTracker::new(vec!["ctrl_l".to_owned(), "shift_l".to_owned()]);
            let t0 = Instant::now();

            // First chord.
            assert_eq!(
                dispatch_raw_event(
                    &guard,
                    &mut tracker,
                    &RawKeyEvent {
                        name: "ctrl_l".to_owned(),
                        kind: RawKeyKind::Press,
                        at: t0,
                    }
                ),
                None
            );
            assert_eq!(
                dispatch_raw_event(
                    &guard,
                    &mut tracker,
                    &RawKeyEvent {
                        name: "shift_l".to_owned(),
                        kind: RawKeyKind::Press,
                        at: t0 + Duration::from_millis(5),
                    }
                ),
                Some(TrackerOutput::ChordPress)
            );
            // Release both.
            assert_eq!(
                dispatch_raw_event(
                    &guard,
                    &mut tracker,
                    &RawKeyEvent {
                        name: "shift_l".to_owned(),
                        kind: RawKeyKind::Release,
                        at: t0 + Duration::from_millis(200),
                    }
                ),
                Some(TrackerOutput::ChordRelease)
            );
            assert_eq!(
                dispatch_raw_event(
                    &guard,
                    &mut tracker,
                    &RawKeyEvent {
                        name: "ctrl_l".to_owned(),
                        kind: RawKeyKind::Release,
                        at: t0 + Duration::from_millis(210),
                    }
                ),
                None
            );

            // Simulate the injection burst reaching the tracker unfiltered
            // (this is what pre-#507 rdev did).
            let t_inject = t0 + Duration::from_millis(500);
            let _ = dispatch_raw_event(
                &guard,
                &mut tracker,
                &RawKeyEvent {
                    name: "__rdev_Unknown(231)".to_owned(),
                    kind: RawKeyKind::Press,
                    at: t_inject,
                },
            );
            // Now the tracker's `pressed` map has a foreign key — the next
            // ctrl_l+shift_l press MUST be blocked by rule 1. That's the
            // wedge condition.
            let t_next = t_inject + Duration::from_millis(300);
            assert_eq!(
                dispatch_raw_event(
                    &guard,
                    &mut tracker,
                    &RawKeyEvent {
                        name: "ctrl_l".to_owned(),
                        kind: RawKeyKind::Press,
                        at: t_next,
                    }
                ),
                None
            );
            // Rule 1 must swallow the completing shift_l press → returns None
            // instead of ChordPress. This is the exact assertion the
            // self-test relies on to declare a failure.
            let second_press = dispatch_raw_event(
                &guard,
                &mut tracker,
                &RawKeyEvent {
                    name: "shift_l".to_owned(),
                    kind: RawKeyKind::Press,
                    at: t_next + Duration::from_millis(5),
                },
            );
            assert_eq!(
                second_press, None,
                "sanity: without the guard, rule 1 blocks the second chord \
                 — this is the wedge condition the self-test detects. If \
                 this assertion fails, the self-test's assumption about \
                 the tracker's behaviour has drifted and the test needs \
                 updating."
            );
        }

        #[test]
        fn iteration_zero_returns_empty_results() {
            let report = run_ptt_wedge_test(0, SelfTestDriver::Rdev);
            assert!(report.all_passed());
            assert_eq!(report.results.len(), 0);
            assert_eq!(report.driver, "rdev");
        }

        #[test]
        fn driver_label_is_reported_in_result() {
            for driver in [
                SelfTestDriver::Auto,
                SelfTestDriver::Rdev,
                SelfTestDriver::Evdev,
            ] {
                let report = run_ptt_wedge_test(1, driver);
                assert_eq!(report.driver, driver.label());
            }
        }

        /// If any iteration would fail, the loop must stop there — a run of
        /// 5 iterations where iteration 3 failed reports 3 results, not 5.
        /// Guarantees the plain-text/json report's `failed_at` is
        /// unambiguous (only one iteration ever fails per report).
        ///
        /// We can't easily break the shipping guard from a test, so this
        /// test exercises the loop's stop condition by consuming an
        /// artificially-constructed report. It's a shape test, not a
        /// wedge-behaviour test — the wedge behaviour is covered by
        /// `without_guard_bracketing_wedge_is_detected`.
        #[test]
        fn report_shape_after_a_synthetic_failure() {
            let r = SelfTestReport {
                iterations: 5,
                driver: "auto",
                results: vec![
                    IterationResult {
                        index: 1,
                        failed_at: None,
                        detail: String::new(),
                    },
                    IterationResult {
                        index: 2,
                        failed_at: None,
                        detail: String::new(),
                    },
                    IterationResult {
                        index: 3,
                        failed_at: Some(WedgeStage::SecondChordPress),
                        detail: "wedge".to_owned(),
                    },
                ],
            };
            assert!(!r.all_passed());
            let json: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
            assert_eq!(json["results"].as_array().unwrap().len(), 3);
            assert_eq!(json["iterations"], 5);
        }
    }
}
