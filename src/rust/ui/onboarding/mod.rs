//! First-run onboarding wizard (Issue #328).
//!
//! High-level shape:
//! - [`wizard`]      — the pure [`wizard::WizardState`] state machine, plus the
//!   `should_trigger_first_run` / `format_seen_at` helpers.
//! - [`steps`]       — the per-step copy ([`steps::StepContent`]) rendered by
//!   the wizard body.
//! - [`permissions`] — the per-OS accessibility / input-monitoring guide the
//!   Permissions step embeds.
//! - This file        — the [`OnboardingUi`] session container that the app
//!   holds on `WhisperDictateApp`, plus [`render_onboarding_modal`], the egui
//!   entry point.
//!
//! The wizard renders as a modal `egui::Window` painted OVER the main panel.
//! We deliberately do NOT spawn a second viewport: (a) the wizard is
//! short-lived (a few frames per session at most), (b) modal-ness is easier
//! to reason about within a single viewport, and (c) the level meter shown
//! in the mic step reads much better next to the main app.

pub mod permissions;
pub mod steps;
pub mod wizard;

use eframe::egui;

// The two helpers below are used from other UI code (`ui.rs` bootstraps the
// wizard first-run gate; `ui/app.rs` reads the seen-at timestamp). Other
// items are only reached from within this module (or its submodules), so no
// re-export is needed and the `-D warnings` clippy leg stays green.
pub use wizard::{format_seen_at, should_trigger_first_run};

use permissions::{guide_for, OsTarget};
use steps::StepContent;
use wizard::{Step, WizardState};

/// Container for the wizard's session state and render bookkeeping. Owned by
/// `WhisperDictateApp` behind an `Option` so `None` means "no wizard active".
#[derive(Debug, Clone)]
pub struct OnboardingUi {
    /// The state machine driving what the wizard shows.
    pub state: WizardState,
    /// Which OS's permission guide to show. Defaults to the running OS but
    /// the wizard exposes a small picker so the user can preview another
    /// (useful when preparing a remote install).
    pub os_target: OsTarget,
    /// User-visible checkbox state for "Don't show this again" on the Skip
    /// button. Tracked here (not inside [`WizardState`]) so the wizard's
    /// dismissal machinery can read it at click time.
    pub dont_show_again: bool,
}

impl Default for OnboardingUi {
    fn default() -> Self {
        Self {
            state: WizardState::new(),
            os_target: OsTarget::current(),
            dont_show_again: false,
        }
    }
}

impl OnboardingUi {
    /// Fresh wizard state for a first-run launch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Re-open the wizard from Settings ("Run setup again"). Always resets
    /// the flow to the welcome step.
    pub fn reopen() -> Self {
        Self {
            state: WizardState::reopen(),
            os_target: OsTarget::current(),
            dont_show_again: false,
        }
    }

    /// Whether the wizard should be visible on screen.
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }
}

/// Outcome of a single wizard render pass. The caller inspects this to decide
/// whether to persist settings (on `PersistCompletion`) and/or drop the
/// [`OnboardingUi`] state (on `Dismissed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingOutcome {
    /// The wizard is still visible; the caller keeps the state around.
    Active,
    /// The user dismissed the wizard this frame WITHOUT ticking "don't show
    /// again" — the caller drops the session state but must NOT flip the
    /// `onboarding_completed` settings flag (so the wizard re-triggers on
    /// next launch).
    DismissedTransient,
    /// The user finished the wizard (or skipped with "don't show again"
    /// ticked). The caller must set `settings.onboarding_completed = true`
    /// and save.
    PersistCompletion,
}

/// Render the wizard as a modal window. Returns an [`OnboardingOutcome`]
/// describing what the caller must do after the frame.
///
/// This is deliberately a free function (not a method on `WhisperDictateApp`)
/// so it can be unit-tested by driving [`WizardState`] directly and calling
/// [`decide_outcome`] — the pure decision function underneath.
///
/// # Issue #459 fix
///
/// v1.20.0 shipped this as an `egui::Window` with `.movable(false)` and
/// `.anchor(CENTER_CENTER)`. That combination interacts poorly with the
/// eframe 0.35 `App::ui(&mut Ui, ..)` dispatch: the containing `Ui`
/// (root background layer) plus a fixed-anchor `Window` on the `Middle`
/// layer left the wizard visible but users reported that its buttons
/// refused to advance the state machine. Independent of that, the fixed
/// anchor + no scroll area meant tall steps (Permissions with the full
/// per-OS guide) pushed the "Next" button off-screen with no way to
/// recover (movable was off), matching the "cannot even move the window"
/// complaint.
///
/// The fix uses [`egui::Modal`] — the canonical modal primitive on
/// egui 0.35+, which:
///   - always paints on `Order::Foreground` so nothing steals its clicks,
///   - installs a click-blocking backdrop so panels below can't fight it,
///   - centers itself without needing a manual anchor,
///
/// and wraps the body in a `ScrollArea` so no step can hide the button
/// row at the bottom. A trailing `ctx.request_repaint()` after any state
/// change guarantees the next step is painted the same tick.
pub fn render_onboarding_modal(
    ctx: &egui::Context,
    ui_state: &mut OnboardingUi,
) -> OnboardingOutcome {
    // Fast path: nothing to do if the state machine already reported the
    // wizard is dismissed.
    if !ui_state.is_active() {
        return decide_outcome(&ui_state.state);
    }

    let mut close_intent: Option<CloseIntent> = None;

    // Cap the wizard body height so the button row is guaranteed
    // reachable even on small windows / tall steps (Permissions). The
    // remaining vertical space is soaked up by the `ScrollArea` below.
    let content_rect = ctx.content_rect();
    let max_body_height = (content_rect.height() * 0.55).max(240.0);
    let min_width = 560.0_f32.min(content_rect.width() - 40.0).max(320.0);

    let modal = egui::Modal::new(egui::Id::new("onboarding_wizard_modal")).show(ctx, |ui| {
        ui.set_min_width(min_width);
        let content = StepContent::for_step(ui_state.state.current);
        draw_step_header(ui, ui_state.state.current, content);
        ui.separator();
        ui.add_space(6.0);

        egui::ScrollArea::vertical()
            .max_height(max_body_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                // Body copy — flowing paragraph.
                ui.label(egui::RichText::new(content.body).size(14.0));
                ui.add_space(6.0);

                // The Permissions step embeds the per-OS guide inline.
                if ui_state.state.current == Step::Permissions {
                    draw_permissions_body(ui, &mut ui_state.os_target);
                }

                if !content.skip_hint.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!("Skip note: {}", content.skip_hint))
                            .italics()
                            .weak(),
                    );
                }
            });

        ui.add_space(12.0);
        ui.separator();
        close_intent = draw_step_controls(ui, ui_state, content);
    });

    if let Some(intent) = close_intent {
        commit_close_intent(ctx, &mut ui_state.state, intent, ui_state.dont_show_again);
    } else if modal.should_close() {
        // egui 0.35's Modal treats Esc / backdrop-click as "should close";
        // we interpret that as a bare skip (transient dismiss) so the
        // wizard re-triggers on next launch. Same intent as clicking
        // "Skip" without the checkbox.
        commit_close_intent(ctx, &mut ui_state.state, CloseIntent::Skip, false);
    }
    decide_outcome(&ui_state.state)
}

/// Apply a [`CloseIntent`] to the wizard state and wake egui so the next
/// step is painted on the next frame. Split out so the regression test for
/// Issue #459 can pin the "state change ⇒ repaint requested" invariant
/// without simulating a click through the render layer.
///
/// # Why the repaint request matters
///
/// A state change on frame N must be visible on frame N+1. Without the
/// explicit [`Context::request_repaint`] the wizard button click can be
/// registered — `state.current` advances — but egui may not schedule
/// another paint pass until some *other* interaction wakes it, which
/// reads to the user as "the wizard is stuck on the first screen." This
/// was the v1.20.0 symptom in Issue #459.
pub fn commit_close_intent(
    ctx: &egui::Context,
    state: &mut WizardState,
    intent: CloseIntent,
    dont_show_again: bool,
) {
    apply_close_intent(state, intent, dont_show_again);
    ctx.request_repaint();
}

fn draw_step_header(ui: &mut egui::Ui, step: Step, content: StepContent) {
    ui.horizontal(|ui| {
        ui.heading(content.heading);
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(format!("Step {} of {}", step.position(), Step::total()))
                .weak()
                .small(),
        );
    });
}

fn draw_permissions_body(ui: &mut egui::Ui, os_target: &mut OsTarget) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Guide for:").strong());
        for target in [
            OsTarget::Windows,
            OsTarget::MacOs,
            OsTarget::LinuxX11,
            OsTarget::LinuxWayland,
        ] {
            if ui
                .selectable_label(*os_target == target, target.label())
                .clicked()
            {
                *os_target = target;
            }
        }
    });
    ui.add_space(6.0);
    for step in guide_for(*os_target) {
        ui.group(|ui| {
            ui.label(egui::RichText::new(step.title).strong());
            ui.label(step.body.as_ref());
            if let Some(link) = step.deep_link {
                if ui
                    .link(egui::RichText::new(format!("Open: {link}")).small())
                    .clicked()
                {
                    // Deep-link handling reuses the existing platform helper.
                    let _ = super::open_url(link);
                }
            }
            ui.label(
                egui::RichText::new(format!("If skipped: {}", step.skip_consequence))
                    .italics()
                    .weak(),
            );
        });
        ui.add_space(4.0);
    }
}

/// What the user just clicked. Split out from the render code so
/// [`apply_close_intent`] is a pure function on the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseIntent {
    /// User clicked "Back".
    Back,
    /// User clicked "Next" or "Finish".
    Advance,
    /// User clicked "Skip".
    Skip,
}

fn draw_step_controls(
    ui: &mut egui::Ui,
    ui_state: &mut OnboardingUi,
    content: StepContent,
) -> Option<CloseIntent> {
    let mut intent = None;
    ui.horizontal(|ui| {
        if ui_state.state.current != Step::Welcome && ui.button("Back").clicked() {
            intent = Some(CloseIntent::Back);
        }
        if content.allow_skip {
            ui.checkbox(&mut ui_state.dont_show_again, "Don't show again");
            if ui.button("Skip").clicked() {
                intent = Some(CloseIntent::Skip);
            }
        }
        // Right-align the primary button.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(content.primary_label).clicked() {
                intent = Some(CloseIntent::Advance);
            }
        });
    });
    intent
}

/// Pure function applying a user click to the state machine. The wall of
/// state transitions lives here (not inline in the render code) so tests can
/// exercise every button combination without a `Ui`.
pub fn apply_close_intent(state: &mut WizardState, intent: CloseIntent, dont_show_again: bool) {
    match intent {
        CloseIntent::Back => state.go_back(),
        CloseIntent::Skip => state.skip(dont_show_again),
        CloseIntent::Advance => {
            if state.current == Step::Done {
                state.finish();
            } else {
                state.advance();
            }
        }
    }
}

/// Pure decision function: given the wizard state, what should the caller do
/// this frame? Split out for testability.
pub fn decide_outcome(state: &WizardState) -> OnboardingOutcome {
    if state.is_active() {
        OnboardingOutcome::Active
    } else if state.should_persist_completion() {
        OnboardingOutcome::PersistCompletion
    } else {
        OnboardingOutcome::DismissedTransient
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_ui_default_is_active_on_welcome() {
        let ui_state = OnboardingUi::default();
        assert!(ui_state.is_active());
        assert_eq!(ui_state.state.current, Step::Welcome);
        assert!(!ui_state.dont_show_again);
    }

    #[test]
    fn onboarding_ui_reopen_resets_to_welcome() {
        // Dirty a wizard mid-flow (progress + checkbox), then confirm reopen()
        // hands out a wholly fresh instance. The mutations here are read by
        // the assertions on `ui_state` below (and are also what makes the
        // reopen contrast meaningful), so they must land BEFORE the reopen()
        // snapshot is taken.
        let mut ui_state = OnboardingUi::default();
        ui_state.state.jump_to(Step::Permissions);
        ui_state.dont_show_again = true;
        assert_eq!(ui_state.state.current, Step::Permissions);
        assert!(ui_state.dont_show_again);

        let reopened = OnboardingUi::reopen();
        assert_eq!(reopened.state.current, Step::Welcome);
        assert!(!reopened.dont_show_again);
    }

    #[test]
    fn advance_intent_walks_through_steps_then_finishes_on_done() {
        // The "happy path": user hits Next on every screen and Finish on the
        // Done screen. The outcome must be PersistCompletion.
        let mut state = WizardState::new();
        for _ in 0..(Step::total() - 1) {
            apply_close_intent(&mut state, CloseIntent::Advance, false);
        }
        assert_eq!(state.current, Step::Done);
        assert!(state.is_active(), "still active until Finish clicked");
        apply_close_intent(&mut state, CloseIntent::Advance, false);
        assert!(!state.is_active());
        assert_eq!(decide_outcome(&state), OnboardingOutcome::PersistCompletion);
    }

    #[test]
    fn back_intent_retreats_but_does_not_dismiss() {
        let mut state = WizardState::new();
        apply_close_intent(&mut state, CloseIntent::Advance, false);
        apply_close_intent(&mut state, CloseIntent::Advance, false);
        assert_eq!(state.current, Step::Hotkey);
        apply_close_intent(&mut state, CloseIntent::Back, false);
        assert_eq!(state.current, Step::Microphone);
        assert!(state.is_active());
        assert_eq!(decide_outcome(&state), OnboardingOutcome::Active);
    }

    #[test]
    fn skip_without_checkbox_is_a_transient_dismiss() {
        // Bare skip — the settings flag must NOT be flipped, so next launch
        // shows the wizard again.
        let mut state = WizardState::new();
        apply_close_intent(&mut state, CloseIntent::Skip, false);
        assert_eq!(
            decide_outcome(&state),
            OnboardingOutcome::DismissedTransient
        );
    }

    #[test]
    fn skip_with_checkbox_persists_completion() {
        let mut state = WizardState::new();
        apply_close_intent(&mut state, CloseIntent::Skip, true);
        assert_eq!(decide_outcome(&state), OnboardingOutcome::PersistCompletion);
    }

    #[test]
    fn decide_outcome_is_active_by_default() {
        let state = WizardState::new();
        assert_eq!(decide_outcome(&state), OnboardingOutcome::Active);
    }

    // --- Issue #459 regression tests -----------------------------------
    //
    // v1.20.0 rendered the wizard as an `egui::Window` with `.movable(false)`
    // and `.anchor(CENTER_CENTER)`. Users reported the wizard visible on
    // the Welcome step but the primary button refusing to advance the
    // state machine. The four tests below pin the invariants the fix
    // relies on so a future refactor can't quietly re-introduce the bug.

    /// Given a real `egui::Context`, driving `commit_close_intent(Advance)`
    /// from the Welcome step must move `current` to `Microphone` AND ask
    /// egui to repaint on the next tick. The missing repaint is the exact
    /// mechanism reported in #459: state advanced but the wizard was still
    /// painted on Welcome because no frame was scheduled after the click.
    #[test]
    fn commit_close_intent_advance_moves_step_and_requests_repaint() {
        // Regression for #459: on v1.20.0, clicking "Get started" advanced
        // `state.current` but no repaint was scheduled, so the wizard was
        // still painted on Welcome until *some other* event woke egui.
        //
        // We can't tell whether the fix restores repaint scheduling by
        // reading `has_requested_repaint()` right after
        // `commit_close_intent`: `Context::default()` starts out with a
        // bootstrap repaint pending, so the flag would trivially be true.
        // What we can measure is the *pass-scoped* repaint request the
        // fix emits: run a no-op pass to consume the bootstrap flag, then
        // run a pass whose ONLY user code is `commit_close_intent`, and
        // observe that egui reports a repaint requested from THIS pass
        // via `requested_repaint_last_pass()`.
        let ctx = egui::Context::default();

        // Bootstrap pass: absorb any startup repaint request so the next
        // pass's flag reflects only our action.
        ctx.begin_pass(egui::RawInput::default());
        let _ = ctx.end_pass();

        let mut state = WizardState::new();
        assert_eq!(state.current, Step::Welcome);

        // Real pass: only user code is the wizard's state commit.
        ctx.begin_pass(egui::RawInput::default());
        commit_close_intent(&ctx, &mut state, CloseIntent::Advance, false);
        let _ = ctx.end_pass();

        assert_eq!(
            state.current,
            Step::Microphone,
            "Advance from Welcome must land on Microphone"
        );
        assert!(
            ctx.requested_repaint_last_pass(),
            "commit_close_intent must wake egui so the new step is painted (#459)"
        );
    }

    /// Rendering the wizard modal through a live `egui::Context` must not
    /// panic on any step, and must return `Active` while the user hasn't
    /// dismissed. Prior to the fix the render used a `Window` with
    /// `movable(false) + anchor(...)` that painted but ate its own clicks;
    /// this smoke test would still have passed, but it also would catch
    /// any *future* attempt to swap Modal for something that panics on
    /// tall content (e.g. the Permissions step).
    #[test]
    fn render_onboarding_modal_paints_every_step_without_panic() {
        let ctx = egui::Context::default();
        // Give the modal a real viewport to lay out inside. The Modal
        // uses `content_rect()`; without a screen_rect it defaults to a
        // zero rect and the ScrollArea's max_height would collapse.
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };

        for &step in Step::ALL.iter() {
            let mut ui_state = OnboardingUi {
                state: WizardState::resume_from(step),
                os_target: permissions::OsTarget::LinuxX11,
                dont_show_again: false,
            };
            ctx.begin_pass(input.clone());
            let observed_outcome = render_onboarding_modal(&ctx, &mut ui_state);
            let _ = ctx.end_pass();
            assert_eq!(
                observed_outcome,
                OnboardingOutcome::Active,
                "step {step:?}: render must report Active while state.is_active()"
            );
            assert_eq!(
                ui_state.state.current, step,
                "step {step:?}: rendering without any click must NOT advance the state"
            );
        }
    }

    /// The full happy path via the render-layer entry point: for each
    /// mid-flow step we simulate the primary button click by feeding an
    /// Advance intent to `commit_close_intent`, then re-render. State
    /// must reach Done and the caller must observe `PersistCompletion`
    /// on the Finish click.
    #[test]
    fn commit_close_intent_walks_wizard_from_welcome_to_persist_completion() {
        let ctx = egui::Context::default();
        let mut state = WizardState::new();
        for _ in 0..(Step::total() - 1) {
            commit_close_intent(&ctx, &mut state, CloseIntent::Advance, false);
        }
        assert_eq!(state.current, Step::Done);
        assert!(state.is_active(), "still active until Finish is committed");
        commit_close_intent(&ctx, &mut state, CloseIntent::Advance, false);
        assert!(state.finished);
        assert_eq!(decide_outcome(&state), OnboardingOutcome::PersistCompletion);
    }

    #[test]
    fn resume_from_mid_flow_then_finish_persists_completion() {
        // "Resume from step 3 after a crash": the previous session died on
        // Backend; the next launch resumes there and the user finishes.
        let mut state = WizardState::resume_from(Step::Backend);
        assert_eq!(state.current, Step::Backend);
        for _ in 0..(Step::total()) {
            apply_close_intent(&mut state, CloseIntent::Advance, false);
        }
        assert_eq!(state.current, Step::Done);
        assert!(state.finished);
        assert_eq!(decide_outcome(&state), OnboardingOutcome::PersistCompletion);
    }
}
