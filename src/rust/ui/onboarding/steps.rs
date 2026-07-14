//! Per-step content model for the onboarding wizard (Issue #328).
//!
//! Every wizard [`super::wizard::Step`] maps to a [`StepContent`] describing
//! its heading, hint copy, and the "what happens if you skip this" line the
//! issue explicitly asks for. Copy is centralized here so the render code in
//! `mod.rs` is a thin loop, and so tests can lock down the contract without
//! spinning up egui.
//!
//! Everything in this module is pure data + trivial mappings — the actual
//! interactive controls (mic picker, hotkey binder, backend picker) live in
//! the existing settings tabs; the wizard just guides the user to them.

use super::wizard::Step;

/// The presentational payload for a single wizard step. Deliberately a plain
/// struct so it's cheap to test and so future translations can live in a
/// lookup table without changing the render code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepContent {
    /// The `H1` heading shown at the top of the step body.
    pub heading: &'static str,
    /// A short paragraph placed under the heading. Rendered as flowing text
    /// so it wraps naturally at the wizard width.
    pub body: &'static str,
    /// The "what happens if you skip this step" line the issue asks for.
    /// Rendered in a muted colour under the primary body copy.
    pub skip_hint: &'static str,
    /// The label of the primary "advance" button. Defaults to "Next" for
    /// mid-flow steps and switches to "Finish" on the [`Step::Done`] step so
    /// the completion is visually explicit.
    pub primary_label: &'static str,
    /// Whether the step gets a "Skip" button. `false` on [`Step::Welcome`]
    /// (there's nothing to skip on the intro) and on [`Step::Done`] (the
    /// user has already committed).
    pub allow_skip: bool,
}

impl StepContent {
    /// Static content for a given step.
    pub fn for_step(step: Step) -> Self {
        match step {
            Step::Welcome => Self {
                heading: "Welcome to whisper-dictate",
                body: "This short setup walks you through the four unavoidable pieces \
                       of a working install: pick your microphone, bind a push-to-talk \
                       key, grant OS-level accessibility permissions, and confirm end-\
                       to-end that a spoken phrase ends up in the focused app.",
                skip_hint: "You can re-open this wizard any time from the System tab.",
                primary_label: "Get started",
                allow_skip: true,
            },
            Step::Microphone => Self {
                heading: "Pick your microphone",
                body: "Choose the input device you want to dictate through. The picker \
                       below shows every device the operating system reports, plus a \
                       live meter so you can confirm you\u{2019}re on the right one.",
                skip_hint: "Without a chosen device the worker uses the OS default \
                            \u{2014} usually fine, but not always the mic you actually want.",
                primary_label: "Next",
                allow_skip: true,
            },
            Step::Hotkey => Self {
                heading: "Bind your push-to-talk key",
                body: "Press the key (or combo) you want to hold to talk. \
                       Right-Ctrl is the default because it\u{2019}s reachable one-handed \
                       and rarely conflicts with editors. You can also switch to \
                       toggle-mode from the Speech tab later.",
                skip_hint: "Without a bound key the worker starts but can\u{2019}t hear the \
                            push-to-talk gesture.",
                primary_label: "Next",
                allow_skip: true,
            },
            Step::Backend => Self {
                heading: "Choose a speech backend",
                body: "Pick \u{201C}Local Whisper\u{201D} for offline dictation (needs a \
                       model download once), or \u{201C}Cloud STT\u{201D} for near-instant \
                       transcription via Groq / OpenAI (needs an API key you can paste \
                       on the next screen).",
                skip_hint: "The default is Local Whisper with the large-v3-turbo model \
                            \u{2014} accurate but downloads ~1.8 GB on first run.",
                primary_label: "Next",
                allow_skip: true,
            },
            Step::DownloadModel => Self {
                heading: "Download a Whisper model",
                body: "The Local Whisper backend needs a whisper.cpp GGML model on \
                       disk. Pick one below and click Download \u{2014} the file lands \
                       under the user cache and the runtime resolves it \
                       automatically.\n\n\
                       Skip this if you plan to use Cloud STT (Groq / OpenAI) \
                       or already have a model downloaded.",
                skip_hint: "Without a Whisper model on disk the local backend \
                            can\u{2019}t start (\u{201C}VOICEPI_WHISPER_MODEL_PATH \
                            unset\u{201D} error on first PTT). You can always \
                            download one later from Settings \u{2192} Speech.",
                primary_label: "Next",
                allow_skip: true,
            },
            Step::Permissions => Self {
                heading: "Grant accessibility permissions",
                body: "Modern operating systems block global hotkeys and keystroke \
                       injection by default. The panel below shows the exact toggles \
                       you need for your OS \u{2014} deep-links open the right pane in \
                       System Settings where possible.",
                skip_hint: "Skipping is fine, but push-to-talk / typing may silently \
                            fail until you enable the toggles later.",
                primary_label: "Next",
                allow_skip: true,
            },
            Step::TestRecording => Self {
                heading: "Test recording",
                body: "Focus a text field (any app), then press-and-hold your push-to-\
                       talk key and say a short phrase. When you release the key, the \
                       transcription should type into the focused field.",
                skip_hint: "The wizard skips the end-to-end confirmation \u{2014} you can \
                            still finish, but any silent failure won\u{2019}t surface until \
                            first real use.",
                primary_label: "Next",
                allow_skip: true,
            },
            Step::Done => Self {
                heading: "You\u{2019}re all set",
                body: "whisper-dictate is now configured. Fine-tune models, dictionary \
                       terms and post-processing from the tabs on the left \u{2014} or \
                       re-open this wizard from the System tab if you change machines \
                       or microphones.",
                skip_hint: "",
                primary_label: "Finish",
                allow_skip: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_has_non_empty_heading_and_body() {
        // Missing copy would render as a blank wizard pane — an obvious
        // regression, but only visible at run time, so we lock it here.
        for &step in Step::ALL.iter() {
            let content = StepContent::for_step(step);
            assert!(
                !content.heading.trim().is_empty(),
                "{step:?} heading is empty"
            );
            assert!(!content.body.trim().is_empty(), "{step:?} body is empty");
            assert!(
                !content.primary_label.trim().is_empty(),
                "{step:?} primary_label is empty"
            );
        }
    }

    #[test]
    fn done_step_uses_finish_label_and_disables_skip() {
        // The Done step is the "commit" moment; a Skip button here would be
        // ambiguous (skip the wizard? skip the completion?), so we hide it.
        let done = StepContent::for_step(Step::Done);
        assert_eq!(done.primary_label, "Finish");
        assert!(!done.allow_skip);
        assert!(
            done.skip_hint.is_empty(),
            "Done step must not show a skip hint"
        );
    }

    #[test]
    fn mid_flow_steps_offer_a_skip_button_and_explain_the_consequence() {
        // Issue #328 is explicit: "Each step shows what changes if it's
        // skipped." Every skippable step must therefore ship a non-empty
        // skip_hint.
        for &step in Step::ALL.iter() {
            let content = StepContent::for_step(step);
            if content.allow_skip {
                assert!(
                    !content.skip_hint.trim().is_empty(),
                    "{step:?} allows skip but has no skip_hint"
                );
            }
        }
    }

    #[test]
    fn welcome_and_done_use_distinct_primary_labels() {
        // The intro and the exit both use larger call-to-action buttons; if
        // they collide the wizard reads as an infinite loop.
        assert_ne!(
            StepContent::for_step(Step::Welcome).primary_label,
            StepContent::for_step(Step::Done).primary_label,
        );
    }
}
