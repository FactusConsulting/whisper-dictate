//! Session-only guided verification for a configured push-to-talk chord.
//!
//! The verifier owns the same native listener used by dictation, but its
//! action sink only records chord-level press/release/cancel signals. It never
//! opens audio, loads a model, transcribes, or injects text.

#![cfg_attr(not(any(feature = "rust-hotkeys", test)), allow(dead_code))]

#[cfg(test)]
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

#[cfg(feature = "rust-hotkeys")]
use crate::runtime::hotkey_probe::{HotkeyProbe, HotkeyProbeEvent, HotkeyProbeSignal};
use crate::ui::canonical_hotkey;

pub(in crate::ui) type HotkeyVerificationFocusSnapshot =
    Arc<dyn Fn() -> Option<bool> + Send + Sync + 'static>;

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
#[cfg_attr(not(feature = "rust-hotkeys"), allow(dead_code))]
pub(in crate::ui) enum HotkeyVerificationSignal {
    Press,
    Release,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub(in crate::ui) struct ObservedHotkeyVerificationSignal {
    pub(in crate::ui) signal: HotkeyVerificationSignal,
    pub(in crate::ui) focused: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct HotkeyVerificationReport {
    pub(in crate::ui) chord: String,
    pub(in crate::ui) driver: String,
    pub(in crate::ui) other_window: HotkeyVerificationOutcome,
    pub(in crate::ui) whisper_dictate: HotkeyVerificationOutcome,
    listener_installed: bool,
    current: Option<HotkeyFocusContext>,
    press_context: Option<HotkeyFocusContext>,
}

impl HotkeyVerificationReport {
    pub(in crate::ui) fn new(chord: String, driver: String) -> Self {
        Self {
            chord: canonical_hotkey(&chord),
            driver,
            other_window: HotkeyVerificationOutcome::Untested,
            whisper_dictate: HotkeyVerificationOutcome::Untested,
            listener_installed: false,
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
        self.listener_installed
            && self.other_window == HotkeyVerificationOutcome::Passed
            && self.whisper_dictate == HotkeyVerificationOutcome::Passed
    }

    pub(in crate::ui) fn listener_installed(&self) -> bool {
        self.listener_installed
    }

    pub(in crate::ui) fn belongs_to(&self, chord: &str) -> bool {
        self.chord == canonical_hotkey(chord)
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

    fn mark_installed(&mut self, driver: String, chord: String) {
        self.driver = driver;
        self.chord = canonical_hotkey(&chord);
        self.listener_installed = true;
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
    #[cfg(any(feature = "rust-hotkeys", test))]
    source: HotkeyVerificationSource,
    report: HotkeyVerificationReport,
    #[cfg(feature = "rust-hotkeys")]
    last_diagnostic: Option<String>,
    failure: Option<String>,
}

#[cfg(any(feature = "rust-hotkeys", test))]
enum HotkeyVerificationSource {
    #[cfg(feature = "rust-hotkeys")]
    Process(HotkeyProbe),
    #[cfg(test)]
    Synthetic(Receiver<ObservedHotkeyVerificationSignal>),
}

#[cfg(any(feature = "rust-hotkeys", test))]
enum IncomingHotkeyVerificationEvent {
    #[cfg(feature = "rust-hotkeys")]
    Installed { driver: String, chord: String },
    Signal {
        signal: HotkeyVerificationSignal,
        focused: Option<bool>,
    },
    #[cfg(feature = "rust-hotkeys")]
    Diagnostic(String),
    #[cfg(feature = "rust-hotkeys")]
    Failed(String),
    #[cfg(feature = "rust-hotkeys")]
    Exited(Option<i32>),
}

#[cfg(any(feature = "rust-hotkeys", test))]
impl HotkeyVerificationSource {
    fn drain(&mut self) -> Vec<IncomingHotkeyVerificationEvent> {
        match self {
            #[cfg(feature = "rust-hotkeys")]
            Self::Process(process) => process
                .poll()
                .into_iter()
                .map(|event| match event {
                    HotkeyProbeEvent::Installed { driver, chord } => {
                        IncomingHotkeyVerificationEvent::Installed { driver, chord }
                    }
                    HotkeyProbeEvent::Signal { signal, focused } => {
                        IncomingHotkeyVerificationEvent::Signal {
                            signal: verification_signal(signal),
                            focused,
                        }
                    }
                    HotkeyProbeEvent::Diagnostic(line) => {
                        IncomingHotkeyVerificationEvent::Diagnostic(line)
                    }
                    HotkeyProbeEvent::Failed(reason) => {
                        IncomingHotkeyVerificationEvent::Failed(reason)
                    }
                    HotkeyProbeEvent::Exited(code) => IncomingHotkeyVerificationEvent::Exited(code),
                })
                .collect(),
            #[cfg(test)]
            Self::Synthetic(rx) => rx
                .try_iter()
                .map(|observed| IncomingHotkeyVerificationEvent::Signal {
                    signal: observed.signal,
                    focused: observed.focused,
                })
                .collect(),
        }
    }

    fn shutdown(self) {
        match self {
            #[cfg(feature = "rust-hotkeys")]
            Self::Process(process) => process.shutdown(),
            #[cfg(test)]
            Self::Synthetic(_) => {}
        }
    }
}

impl HotkeyVerificationSession {
    pub(in crate::ui) fn start(
        chord: &str,
        planned_driver: &str,
        focus_snapshot: HotkeyVerificationFocusSnapshot,
        repaint: crate::runtime::RepaintNotifier,
    ) -> Result<Self, String> {
        #[cfg(not(feature = "rust-hotkeys"))]
        {
            let _ = (chord, planned_driver, focus_snapshot, repaint);
            Err("hotkey diagnostic requires the rust-hotkeys feature".to_owned())
        }
        #[cfg(feature = "rust-hotkeys")]
        {
            let source = HotkeyVerificationSource::Process(HotkeyProbe::spawn(
                &canonical_hotkey(chord),
                planned_driver,
                focus_snapshot,
                repaint,
            )?);
            Ok(Self {
                source,
                report: HotkeyVerificationReport::new(
                    canonical_hotkey(chord),
                    planned_driver.to_owned(),
                ),
                #[cfg(feature = "rust-hotkeys")]
                last_diagnostic: None,
                failure: None,
            })
        }
    }

    pub(in crate::ui) fn report(&self) -> &HotkeyVerificationReport {
        &self.report
    }

    pub(in crate::ui) fn failure_reason(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    #[cfg(any(feature = "rust-hotkeys", test))]
    pub(in crate::ui) fn poll(&mut self) -> bool {
        let mut changed = false;
        for event in self.source.drain() {
            match event {
                #[cfg(feature = "rust-hotkeys")]
                IncomingHotkeyVerificationEvent::Installed { driver, chord } => {
                    self.report.mark_installed(driver, chord);
                    crate::diag::log!(
                        "[hotkey/verify] diagnostic listener installed driver={} chord={} audio=false injection=false",
                        self.report.driver,
                        self.report.chord
                    );
                    changed = true;
                }
                IncomingHotkeyVerificationEvent::Signal { signal, focused } => {
                    let applied = self.report.observe(signal, focused);
                    if crate::diag::debug_enabled() {
                        crate::diag::log!(
                            "[hotkey/verify/debug] received chord signal={signal:?} chord={} viewport_focused={focused:?} applied={applied}",
                            self.report.chord
                        );
                    }
                    changed |= applied;
                }
                #[cfg(feature = "rust-hotkeys")]
                IncomingHotkeyVerificationEvent::Diagnostic(line) => {
                    self.last_diagnostic = Some(line)
                }
                #[cfg(feature = "rust-hotkeys")]
                IncomingHotkeyVerificationEvent::Failed(reason) => self.failure = Some(reason),
                #[cfg(feature = "rust-hotkeys")]
                IncomingHotkeyVerificationEvent::Exited(code) if !self.report.is_complete() => {
                    self.failure = Some(self.last_diagnostic.clone().unwrap_or_else(|| {
                        format!("hotkey diagnostic process exited with code {code:?}")
                    }));
                }
                #[cfg(feature = "rust-hotkeys")]
                IncomingHotkeyVerificationEvent::Exited(_) => {}
            }
        }
        changed
    }

    #[cfg(not(any(feature = "rust-hotkeys", test)))]
    pub(in crate::ui) fn poll(&mut self) -> bool {
        false
    }

    pub(in crate::ui) fn fail_current(&mut self) -> bool {
        let context = self.report.current_context();
        let changed = self.report.fail_current();
        if changed {
            crate::diag::log!("[hotkey/verify] context={context:?} result=failed-or-no-response");
        }
        changed
    }

    pub(in crate::ui) fn shutdown(self) {
        #[cfg(any(feature = "rust-hotkeys", test))]
        self.source.shutdown();
    }

    #[cfg(test)]
    pub(in crate::ui) fn synthetic(
        chord: &str,
        driver: &str,
    ) -> (
        Self,
        std::sync::mpsc::Sender<ObservedHotkeyVerificationSignal>,
    ) {
        let (tx, rx) = mpsc::channel();
        let mut report = HotkeyVerificationReport::new(chord.to_owned(), driver.to_owned());
        report.mark_installed(driver.to_owned(), chord.to_owned());
        (
            Self {
                source: HotkeyVerificationSource::Synthetic(rx),
                report,
                #[cfg(feature = "rust-hotkeys")]
                last_diagnostic: None,
                failure: None,
            },
            tx,
        )
    }
}

#[cfg(feature = "rust-hotkeys")]
fn verification_signal(signal: HotkeyProbeSignal) -> HotkeyVerificationSignal {
    match signal {
        HotkeyProbeSignal::Press => HotkeyVerificationSignal::Press,
        HotkeyProbeSignal::Release => HotkeyVerificationSignal::Release,
        HotkeyProbeSignal::Cancel => HotkeyVerificationSignal::Cancel,
    }
}
