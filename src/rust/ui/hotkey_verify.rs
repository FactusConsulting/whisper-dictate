//! Session-only guided verification for a configured push-to-talk chord.
//!
//! The verifier owns the same native listener used by dictation, but its
//! action sink only records chord-level press/release/cancel signals. It never
//! opens audio, loads a model, transcribes, or injects text.

use std::sync::mpsc::{self, Receiver};

use crate::hotkey::coordinator::CoordinatorAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum HotkeyFocusContext {
    OtherWindow,
    WhisperDictate,
}

impl HotkeyFocusContext {
    pub(in crate::ui) fn label(self) -> &'static str {
        match self {
            Self::OtherWindow => "Another focused window",
            Self::WhisperDictate => "WhisperDictate focused",
        }
    }

    fn from_viewport_focus(focused: Option<bool>) -> Option<Self> {
        focused.map(|focused| {
            if focused {
                Self::WhisperDictate
            } else {
                Self::OtherWindow
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum HotkeyVerificationOutcome {
    Untested,
    Passed,
    Failed,
}

impl HotkeyVerificationOutcome {
    pub(in crate::ui) fn label(self) -> &'static str {
        match self {
            Self::Untested => "not tested",
            Self::Passed => "verified",
            Self::Failed => "failed / no response",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct InstalledHotkeyStatus {
    pub(in crate::ui) chord: String,
    pub(in crate::ui) driver: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum HotkeyVerificationSignal {
    Press,
    Release,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct HotkeyVerificationReport {
    pub(in crate::ui) chord: String,
    pub(in crate::ui) driver: String,
    pub(in crate::ui) other_window: HotkeyVerificationOutcome,
    pub(in crate::ui) whisper_dictate: HotkeyVerificationOutcome,
    current: Option<HotkeyFocusContext>,
    press_context: Option<HotkeyFocusContext>,
}

impl HotkeyVerificationReport {
    pub(in crate::ui) fn new(chord: String, driver: String) -> Self {
        Self {
            chord,
            driver,
            other_window: HotkeyVerificationOutcome::Untested,
            whisper_dictate: HotkeyVerificationOutcome::Untested,
            current: Some(HotkeyFocusContext::OtherWindow),
            press_context: None,
        }
    }

    pub(in crate::ui) fn current_context(&self) -> Option<HotkeyFocusContext> {
        self.current
    }

    pub(in crate::ui) fn is_complete(&self) -> bool {
        self.current.is_none()
    }

    pub(in crate::ui) fn is_verified(&self) -> bool {
        self.other_window == HotkeyVerificationOutcome::Passed
            && self.whisper_dictate == HotkeyVerificationOutcome::Passed
    }

    pub(in crate::ui) fn belongs_to(&self, chord: &str) -> bool {
        self.chord == chord.trim()
    }

    pub(in crate::ui) fn observe(
        &mut self,
        signal: HotkeyVerificationSignal,
        focused: Option<bool>,
    ) -> bool {
        let Some(expected) = self.current else {
            return false;
        };
        let observed = HotkeyFocusContext::from_viewport_focus(focused);
        match signal {
            HotkeyVerificationSignal::Press if observed == Some(expected) => {
                let changed = self.press_context != Some(expected);
                self.press_context = Some(expected);
                changed
            }
            HotkeyVerificationSignal::Release
                if self.press_context == Some(expected) && observed == Some(expected) =>
            {
                self.set_outcome(expected, HotkeyVerificationOutcome::Passed);
                self.advance();
                true
            }
            HotkeyVerificationSignal::Cancel => self.press_context.take().is_some(),
            _ => false,
        }
    }

    pub(in crate::ui) fn fail_current(&mut self) -> bool {
        let Some(current) = self.current else {
            return false;
        };
        self.set_outcome(current, HotkeyVerificationOutcome::Failed);
        self.advance();
        true
    }

    fn set_outcome(&mut self, context: HotkeyFocusContext, outcome: HotkeyVerificationOutcome) {
        match context {
            HotkeyFocusContext::OtherWindow => self.other_window = outcome,
            HotkeyFocusContext::WhisperDictate => self.whisper_dictate = outcome,
        }
    }

    fn advance(&mut self) {
        self.press_context = None;
        self.current = match self.current {
            Some(HotkeyFocusContext::OtherWindow) => Some(HotkeyFocusContext::WhisperDictate),
            Some(HotkeyFocusContext::WhisperDictate) | None => None,
        };
    }
}

pub(in crate::ui) struct HotkeyVerificationSession {
    handle: Option<crate::hotkey::HotkeyHandle>,
    rx: Receiver<HotkeyVerificationSignal>,
    report: HotkeyVerificationReport,
}

impl HotkeyVerificationSession {
    pub(in crate::ui) fn start(
        chord: &str,
        repaint: crate::runtime::RepaintNotifier,
    ) -> Result<Self, String> {
        let key_names = chord
            .split('+')
            .map(str::trim)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let (tx, rx) = mpsc::channel();
        let chord_for_log = chord.trim().to_owned();
        let handle = crate::hotkey::install_hotkey(
            crate::hotkey::HotkeyConfig::hold_to_talk(key_names)
                .with_auto_complete_processing(true),
            move |action| {
                let signal = match action {
                    CoordinatorAction::StartRecording(_) => HotkeyVerificationSignal::Press,
                    CoordinatorAction::StopAndTranscribe(_) => HotkeyVerificationSignal::Release,
                    CoordinatorAction::CancelRecording(_) => HotkeyVerificationSignal::Cancel,
                };
                let signal_label = match signal {
                    HotkeyVerificationSignal::Press => "press",
                    HotkeyVerificationSignal::Release => "release",
                    HotkeyVerificationSignal::Cancel => "cancel",
                };
                if crate::diag::debug_enabled() {
                    crate::diag::log!(
                        "[hotkey/verify/debug] received chord signal={signal_label} chord={chord_for_log}"
                    );
                }
                let _ = tx.send(signal);
                repaint();
            },
        )
        .map_err(|err| err.to_string())?;
        let driver = handle.driver_name().to_owned();
        let chord = chord.trim().to_owned();
        crate::diag::log!(
            "[hotkey/verify] diagnostic listener installed driver={driver} chord={chord} audio=false injection=false"
        );
        Ok(Self {
            handle: Some(handle),
            rx,
            report: HotkeyVerificationReport::new(chord, driver),
        })
    }

    pub(in crate::ui) fn report(&self) -> &HotkeyVerificationReport {
        &self.report
    }

    pub(in crate::ui) fn poll(&mut self, focused: Option<bool>) -> bool {
        let mut changed = false;
        while let Ok(signal) = self.rx.try_recv() {
            let applied = self.report.observe(signal, focused);
            if crate::diag::trace_enabled() {
                crate::diag::log!(
                    "[hotkey/verify/trace] signal={signal:?} viewport_focused={focused:?} applied={applied} context={:?}",
                    self.report.current_context()
                );
            }
            changed |= applied;
        }
        changed
    }

    pub(in crate::ui) fn fail_current(&mut self) -> bool {
        let context = self.report.current_context();
        let changed = self.report.fail_current();
        if changed {
            crate::diag::log!("[hotkey/verify] context={context:?} result=failed-or-no-response");
        }
        changed
    }

    pub(in crate::ui) fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
        }
    }

    #[cfg(test)]
    pub(in crate::ui) fn synthetic(
        chord: &str,
        driver: &str,
    ) -> (Self, std::sync::mpsc::Sender<HotkeyVerificationSignal>) {
        let (tx, rx) = mpsc::channel();
        (
            Self {
                handle: None,
                rx,
                report: HotkeyVerificationReport::new(chord.to_owned(), driver.to_owned()),
            },
            tx,
        )
    }
}
