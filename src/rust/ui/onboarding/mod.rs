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

    egui::Window::new("Welcome to whisper-dictate")
        .collapsible(false)
        .resizable(false)
        .movable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .default_width(560.0)
        .show(ctx, |ui| {
            let content = StepContent::for_step(ui_state.state.current);
            draw_step_header(ui, ui_state.state.current, content);
            ui.separator();
            ui.add_space(6.0);

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

            ui.add_space(12.0);
            ui.separator();
            close_intent = draw_step_controls(ui, ui_state, content);
        });

    if let Some(intent) = close_intent {
        apply_close_intent(&mut ui_state.state, intent, ui_state.dont_show_again);
    }
    decide_outcome(&ui_state.state)
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
