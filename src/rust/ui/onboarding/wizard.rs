//! First-run onboarding wizard state machine (Issue #328).
//!
//! The wizard is a plain finite state machine over the [`Step`] enum plus a
//! small bag of session flags (skip / don't-show-again / finished). It owns
//! **no** egui state; the render code in `steps.rs` reads the enum and calls
//! the transition methods here (`next`, `back`, `skip`, `finish`, `reopen`).
//!
//! Everything in this file is `cfg(any)` — the render layer is heavy, but the
//! logic is cheap to unit-test and covers the "resume from step 3 after a
//! crash" case: [`WizardState::resume_from`] is what the app calls on first
//! frame with a settings-derived starting step.

use crate::config::AppSettings;

/// The wizard steps, in the order the user sees them. Ordering follows the
/// "typical scope" of Issue #328: welcome banner → microphone → hotkey → STT
/// backend selection → download a whisper model → permissions guide → test
/// recording → done screen.
///
/// The permissions guide is kept together with the mic + hotkey steps rather
/// than pushed to the end, so a user who bails halfway through has already
/// seen the OS-level toggles that most often cause "I pressed the key and
/// nothing happened" support tickets.
///
/// `DownloadModel` (bug 3 of the multilingual-catalog PR) sits directly
/// after `Backend` — the local-Whisper picker on Backend is meaningless
/// until a GGML file lives on disk, and prior to this step users hit a
/// "worker won't start" wall the first time they pressed PTT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    /// Welcome banner + short "what this wizard does" copy.
    Welcome,
    /// Microphone picker with live level meter.
    Microphone,
    /// Push-to-talk key binder.
    Hotkey,
    /// STT backend picker (local Whisper vs cloud STT).
    Backend,
    /// Download a Whisper GGML model into the user cache. Skippable for
    /// cloud-only users.
    DownloadModel,
    /// Per-OS accessibility / input-monitoring guide (permissions.rs).
    Permissions,
    /// Test recording — the user presses PTT once and the wizard confirms
    /// the audio + transcription pipeline worked end-to-end.
    TestRecording,
    /// Completion screen: summary + "Finish" button.
    Done,
}

impl Step {
    /// The ordered list of steps, used to compute the "step N of M" progress
    /// indicator and to advance / retreat between steps.
    pub const ALL: [Step; 8] = [
        Step::Welcome,
        Step::Microphone,
        Step::Hotkey,
        Step::Backend,
        Step::DownloadModel,
        Step::Permissions,
        Step::TestRecording,
        Step::Done,
    ];

    /// The step's ordinal position (1-based) for the "Step N of M" pill.
    pub fn position(self) -> usize {
        Self::ALL
            .iter()
            .position(|s| *s == self)
            .expect("Step::ALL must list every variant")
            + 1
    }

    /// Total number of steps (7 today, but callers should read the constant
    /// so re-ordering doesn't leak).
    pub fn total() -> usize {
        Self::ALL.len()
    }

    /// The next step in the sequence, or `None` at the end.
    pub fn next(self) -> Option<Step> {
        let idx = self.position();
        Self::ALL.get(idx).copied()
    }

    /// The previous step in the sequence, or `None` at the start.
    pub fn previous(self) -> Option<Step> {
        let idx = self.position();
        if idx <= 1 {
            None
        } else {
            Self::ALL.get(idx - 2).copied()
        }
    }
}

/// Snapshot of the wizard's session state. Everything is plain data so the
/// state machine can be exercised without an egui context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WizardState {
    /// The step currently shown.
    pub current: Step,
    /// Set to `true` when the user hits "Skip". Kept as a session flag so the
    /// wizard can still show a "You skipped setup — re-open from Settings"
    /// banner on the way out.
    pub skipped: bool,
    /// The "Don't show again" checkbox on the skip button. When `true` and
    /// paired with `skipped=true` (or `finished=true`), the caller persists
    /// `settings.onboarding_completed = true`.
    pub dont_show_again: bool,
    /// Set to `true` when the user completes the whole flow via "Finish" on
    /// the Done step. Mutually exclusive with `skipped` in the sense that the
    /// wizard is dismissed either way, but both flags survive so the caller
    /// can distinguish "user finished" from "user bailed".
    pub finished: bool,
}

impl Default for WizardState {
    fn default() -> Self {
        Self {
            current: Step::Welcome,
            skipped: false,
            dont_show_again: false,
            finished: false,
        }
    }
}

impl WizardState {
    /// Fresh wizard, positioned on the welcome step.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resume the wizard on a specific step. Used by the app when the user
    /// re-opens the wizard from the System tab, or (in a future iteration)
    /// when a crash recovery flag indicates the wizard had progressed past
    /// the welcome screen last time. The public API is deliberately broader
    /// than the current call sites — the wizard is a state machine designed
    /// to be resumable, and locking down the shape of that machine below the
    /// intent (via `#[allow(dead_code)]`) is the point.
    #[allow(dead_code)]
    pub fn resume_from(step: Step) -> Self {
        Self {
            current: step,
            ..Self::default()
        }
    }

    /// Whether the wizard is still active on screen. The caller uses this to
    /// decide whether to render the modal.
    pub fn is_active(&self) -> bool {
        !self.finished && !self.skipped
    }

    /// Advance to the next step. On the last step this is a no-op — the caller
    /// should call [`Self::finish`] instead.
    pub fn advance(&mut self) {
        if let Some(next) = self.current.next() {
            self.current = next;
        }
    }

    /// Retreat to the previous step. On the first step this is a no-op.
    pub fn go_back(&mut self) {
        if let Some(prev) = self.current.previous() {
            self.current = prev;
        }
    }

    /// Jump directly to a step. Reserved for a future step-list sidebar; the
    /// current render code only uses next/back, but the pure state machine
    /// exposes the operation for tests and for future callers.
    #[allow(dead_code)]
    pub fn jump_to(&mut self, step: Step) {
        self.current = step;
    }

    /// Mark the wizard as skipped. `dont_show_again` records the checkbox
    /// state so the caller can decide whether to persist
    /// `onboarding_completed`.
    pub fn skip(&mut self, dont_show_again: bool) {
        self.skipped = true;
        self.dont_show_again = dont_show_again;
    }

    /// Mark the wizard as finished (Done step "Finish" button). Always
    /// persists `onboarding_completed = true` — see
    /// [`Self::should_persist_completion`].
    pub fn finish(&mut self) {
        self.finished = true;
        self.dont_show_again = true;
    }

    /// Whether the caller must flip `settings.onboarding_completed = true`
    /// after this session. `true` when the user finished, or when they
    /// skipped WITH the "don't show again" checkbox ticked. A bare skip
    /// (without the checkbox) leaves the flag alone, so the wizard triggers
    /// again on the next launch.
    pub fn should_persist_completion(&self) -> bool {
        self.finished || (self.skipped && self.dont_show_again)
    }

    /// Re-open the wizard from Settings ("Run setup again"). Resets all
    /// session flags but always starts back at the welcome screen — the user
    /// asked for the full flow, not a quick jump.
    pub fn reopen() -> Self {
        Self::default()
    }
}

/// Whether the wizard should be shown on first frame, based on the loaded
/// settings. Pure function — the app calls this at startup with the freshly
/// loaded `AppSettings`.
///
/// The rule: show the wizard when `onboarding_completed == false`. That flag
/// defaults to `false` on a brand-new install (see `AppSettings::default()`),
/// so first launch always triggers; on subsequent launches it stays `true`
/// after "Finish" or "Skip + don't show again".
pub fn should_trigger_first_run(settings: &AppSettings) -> bool {
    !settings.onboarding_completed
}

/// Timestamp helper: format an RFC 3339 UTC string for `onboarding_seen_at`.
/// Deliberately uses `SystemTime` (no `chrono` dependency — this crate ships
/// without it) and computes the Y-M-D breakdown by hand from a Unix timestamp
/// so the test path stays deterministic.
///
/// The output is second-precision, `Z`-suffixed UTC:
/// `YYYY-MM-DDTHH:MM:SSZ`. Callers persist it verbatim to
/// `settings.onboarding_seen_at`; readers parse it with the standard-library
/// suffix-stripping approach in [`parse_seen_at_unix`] when needed.
pub fn format_seen_at(now: std::time::SystemTime) -> String {
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_unix_seconds_as_rfc3339(secs)
}

/// Convert a UTC Unix timestamp (whole seconds) into the second-precision
/// RFC 3339 shape whisper-dictate persists. Public so unit tests can pin a
/// deterministic instant without a real clock.
pub fn format_unix_seconds_as_rfc3339(secs: u64) -> String {
    let (year, month, day, hh, mm, ss) = unix_seconds_to_ymd_hms(secs);
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Break a UTC Unix timestamp (whole seconds) into (year, month, day, hour,
/// minute, second). Uses the civil_from_days algorithm (Howard Hinnant, 2015)
/// which is exact for the full range of a `u64` epoch.
fn unix_seconds_to_ymd_hms(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let time_of_day = (secs % 86_400) as u32;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;

    // civil_from_days: converts days-since-1970-01-01 into (y, m, d).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, hh, mm, ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_all_contains_every_variant_in_display_order() {
        // Guard against reorderings that would move Welcome away from the
        // front or Done away from the back (the render code assumes both).
        assert_eq!(*Step::ALL.first().unwrap(), Step::Welcome);
        assert_eq!(*Step::ALL.last().unwrap(), Step::Done);
        // Every position() is 1-based and unique.
        let positions: Vec<usize> = Step::ALL.iter().map(|s| s.position()).collect();
        assert_eq!(positions, (1..=Step::ALL.len()).collect::<Vec<_>>());
    }

    #[test]
    fn step_next_and_previous_are_symmetric() {
        // For every step except the endpoints, prev(next(s)) == s and
        // next(prev(s)) == s. The endpoints must return None on the correct
        // side.
        for &step in Step::ALL.iter() {
            if let Some(next) = step.next() {
                assert_eq!(
                    next.previous(),
                    Some(step),
                    "prev(next({step:?})) != {step:?}"
                );
            }
            if let Some(prev) = step.previous() {
                assert_eq!(prev.next(), Some(step), "next(prev({step:?})) != {step:?}");
            }
        }
        assert!(Step::Welcome.previous().is_none());
        assert!(Step::Done.next().is_none());
    }

    #[test]
    fn advance_walks_through_all_steps_and_stops_on_done() {
        let mut state = WizardState::new();
        for expected in Step::ALL.iter().skip(1) {
            state.advance();
            assert_eq!(state.current, *expected);
        }
        // One more advance past Done is a no-op (still on Done).
        state.advance();
        assert_eq!(state.current, Step::Done);
    }

    #[test]
    fn go_back_stops_at_welcome() {
        let mut state = WizardState::new();
        state.advance();
        state.advance();
        assert_eq!(state.current, Step::Hotkey);
        state.go_back();
        state.go_back();
        assert_eq!(state.current, Step::Welcome);
        // Going back further stays on Welcome.
        state.go_back();
        assert_eq!(state.current, Step::Welcome);
    }

    #[test]
    fn skip_sets_flags_and_hides_wizard() {
        // Skip WITHOUT the checkbox — session is dismissed but the flag is
        // NOT persisted, so the wizard re-triggers on next launch.
        let mut state = WizardState::new();
        state.skip(false);
        assert!(state.skipped);
        assert!(!state.dont_show_again);
        assert!(!state.is_active(), "skip must dismiss the wizard");
        assert!(
            !state.should_persist_completion(),
            "bare skip must NOT flip onboarding_completed"
        );
    }

    #[test]
    fn skip_with_dont_show_again_persists_completion() {
        let mut state = WizardState::new();
        state.skip(true);
        assert!(state.skipped);
        assert!(state.dont_show_again);
        assert!(
            state.should_persist_completion(),
            "skip + don't-show-again must flip onboarding_completed"
        );
    }

    #[test]
    fn finish_always_persists_completion() {
        let mut state = WizardState::new();
        // Walk to the Done step to keep the invariant honest — a caller that
        // calls finish() from mid-flow would just short-circuit here.
        for _ in 0..Step::ALL.len() {
            state.advance();
        }
        assert_eq!(state.current, Step::Done);
        state.finish();
        assert!(state.finished);
        assert!(state.dont_show_again);
        assert!(!state.is_active(), "finish must dismiss the wizard");
        assert!(state.should_persist_completion());
    }

    #[test]
    fn resume_from_starts_on_the_requested_step_with_clean_flags() {
        // The "crash recovery" case: the app crashed at step 3
        // (Backend picker) and the next launch resumes there directly, with
        // no lingering skipped/finished flags from the previous session.
        let state = WizardState::resume_from(Step::Backend);
        assert_eq!(state.current, Step::Backend);
        assert!(!state.skipped);
        assert!(!state.finished);
        assert!(!state.dont_show_again);
        assert!(state.is_active());
        // Advancing from a mid-flow resume must land on the correct next step,
        // not restart from Welcome. Bug 3 of the multilingual-catalog PR
        // inserted DownloadModel between Backend and Permissions.
        let mut state = state;
        state.advance();
        assert_eq!(state.current, Step::DownloadModel);
        state.advance();
        assert_eq!(state.current, Step::Permissions);
    }

    #[test]
    fn download_model_step_sits_between_backend_and_permissions() {
        // Bug 3 of the multilingual-catalog PR: the DownloadModel step
        // must sit directly after Backend (the picker only becomes
        // actionable once a model is on disk) and before Permissions (the
        // OS-toggle guide). A regression that moves it would either hide
        // the required next step or push it past the end of the flow.
        assert_eq!(Step::Backend.next(), Some(Step::DownloadModel));
        assert_eq!(Step::DownloadModel.next(), Some(Step::Permissions));
        assert_eq!(Step::DownloadModel.previous(), Some(Step::Backend));
        assert!(
            Step::ALL.contains(&Step::DownloadModel),
            "DownloadModel must be listed in Step::ALL"
        );
    }

    #[test]
    fn reopen_returns_a_clean_wizard_on_the_welcome_step() {
        // Users who click "Run setup again" from Settings get the full flow.
        let state = WizardState::reopen();
        assert_eq!(state.current, Step::Welcome);
        assert!(!state.skipped);
        assert!(!state.finished);
        assert!(!state.dont_show_again);
        assert!(state.is_active());
    }

    #[test]
    fn jump_to_moves_to_arbitrary_step_without_touching_flags() {
        // Used by the future step-list sidebar; must never accidentally
        // flip skipped/finished.
        let mut state = WizardState::new();
        state.jump_to(Step::Permissions);
        assert_eq!(state.current, Step::Permissions);
        assert!(!state.skipped);
        assert!(!state.finished);
    }

    #[test]
    fn should_trigger_first_run_matches_settings_flag() {
        let mut settings = AppSettings::default();
        // Fresh install: default false ⇒ trigger.
        assert!(should_trigger_first_run(&settings));
        settings.onboarding_completed = true;
        assert!(!should_trigger_first_run(&settings));
    }

    #[test]
    fn format_unix_seconds_produces_rfc_3339_utc_string() {
        // Pin a known Unix timestamp so the check is stable across timezones
        // and independent of the wall clock: 2026-07-04T12:34:56Z is
        // Unix time 1_783_168_496 (days 1970..2026-07-04 = 20638,
        // + 12h34m56s).
        let secs = 1_783_168_496u64;
        let formatted = format_unix_seconds_as_rfc3339(secs);
        assert_eq!(formatted, "2026-07-04T12:34:56Z");
        assert!(formatted.ends_with('Z'));
        // The Unix epoch itself must format as 1970-01-01T00:00:00Z (this
        // catches sign / off-by-one bugs in the civil_from_days port).
        assert_eq!(format_unix_seconds_as_rfc3339(0), "1970-01-01T00:00:00Z");
        // A leap-year boundary (Feb 29 2024) — 1709164800 unix seconds is
        // 2024-02-29T00:00:00Z.
        assert_eq!(
            format_unix_seconds_as_rfc3339(1_709_164_800),
            "2024-02-29T00:00:00Z"
        );
    }

    #[test]
    fn format_seen_at_uses_system_time_without_panicking() {
        // The wall-clock formatter must never panic — it's called at
        // wizard-open time on the UI thread. We can't assert an exact string
        // (that's the deterministic sibling), but we can lock down the shape.
        let formatted = format_seen_at(std::time::SystemTime::now());
        assert!(
            formatted.len() == 20,
            "expected `YYYY-MM-DDTHH:MM:SSZ` (20 chars), got {formatted:?}"
        );
        assert!(formatted.ends_with('Z'));
    }

    #[test]
    fn is_active_toggles_off_after_skip_and_finish() {
        // Belt-and-suspenders: the render layer keys off is_active(); a
        // regression that leaves the wizard visible after dismiss would be
        // extremely visible.
        let mut state = WizardState::new();
        assert!(state.is_active());
        state.skip(false);
        assert!(!state.is_active());

        let mut state = WizardState::new();
        state.finish();
        assert!(!state.is_active());
    }
}
