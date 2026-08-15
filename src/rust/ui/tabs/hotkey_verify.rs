use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn hotkey_verification_controls(
        &mut self,
        ui: &mut egui::Ui,
        palette: UiPalette,
    ) {
        let active = self
            .hotkey_verification_session
            .as_ref()
            .map(|session| session.report().clone());
        let last = self.hotkey_verification.clone();
        let focus_attribution_available =
            crate::platform::foreground_window::focus_attribution_available();
        let can_start = self.runtime_state == RuntimeState::Stopped
            && !self.supervisor.is_teardown_pending()
            && guided_hotkey_verification_available(
                &self.settings.key,
                focus_attribution_available,
            );
        let mut start_requested = false;
        let mut fail_requested = false;
        let mut stop_requested = false;

        ui.vertical(|ui| {
            if let Some(report) = active.as_ref() {
                let listener_status = if report.listener_installed() {
                    format!(
                        "Diagnostic listener installed: {} ({})",
                        report.driver, report.chord
                    )
                } else {
                    format!(
                        "Starting diagnostic listener: planned {} ({})",
                        report.driver, report.chord
                    )
                };
                ui.label(
                    egui::RichText::new(listener_status)
                        .strong()
                        .color(palette.accent_blue),
                );
                ui.label(format!(
                    "Another focused window: {}",
                    report.other_window.label()
                ));
                ui.label(format!(
                    "WhisperDictate focused: {}",
                    report.whisper_dictate.label()
                ));
                if !report.listener_installed() {
                    ui.label(
                        egui::RichText::new(
                            "Wait for the diagnostic listener to install before pressing the shortcut.",
                        )
                        .color(palette.text_muted),
                    );
                    if ui.button("Stop test").clicked() {
                        stop_requested = true;
                    }
                } else if let Some(context) = report.actionable_context() {
                    let instruction = match context {
                        HotkeyFocusContext::OtherWindow => {
                            "Step 1/2: focus another application, then press and release the shortcut. Return here if nothing happens."
                        }
                        HotkeyFocusContext::WhisperDictate => {
                            "Step 2/2: keep WhisperDictate focused, then press and release the shortcut."
                        }
                    };
                    ui.label(egui::RichText::new(instruction).color(palette.text));
                    ui.horizontal(|ui| {
                        if ui
                            .button(format!("No response - mark {} failed", context.label()))
                            .clicked()
                        {
                            fail_requested = true;
                        }
                        if ui.button("Stop test").clicked() {
                            stop_requested = true;
                        }
                    });
                }
            } else {
                if ui
                    .add_enabled(can_start, egui::Button::new("Test shortcut in both windows"))
                    .clicked()
                {
                    start_requested = true;
                }
                ui.label(
                    egui::RichText::new(
                        "The guided test installs only the hotkey listener: no microphone, model, transcription, or text injection.",
                    )
                    .color(palette.text_muted),
                );
                if self.runtime_state != RuntimeState::Stopped {
                    ui.label(
                        egui::RichText::new("Stop dictation before testing the shortcut.")
                            .color(palette.warn_text),
                    );
                } else if !focus_attribution_available {
                    ui.label(
                        egui::RichText::new(
                            "Focus-aware shortcut verification is unavailable in this desktop session.",
                        )
                        .color(palette.warn_text),
                    );
                }
                if let Some(report) = last.as_ref() {
                    let stale = !report.belongs_to(&self.settings.key);
                    let headline = if stale {
                        format!(
                            "Previous diagnostic is for {} ({}) and does not verify the edited chord.",
                            report.chord, report.driver
                        )
                    } else if report.is_verified() {
                        format!("Verified in both focus contexts with {}.", report.driver)
                    } else if report.is_complete() {
                        "Not verified in both contexts; use `pause` or test another chord."
                            .to_owned()
                    } else {
                        "Previous diagnostic was stopped before both contexts completed."
                            .to_owned()
                    };
                    let color = if !stale && report.is_verified() {
                        palette.ok_text
                    } else {
                        palette.warn_text
                    };
                    ui.label(egui::RichText::new(headline).color(color));
                    ui.label(format!(
                        "Another focused window: {}; WhisperDictate focused: {}",
                        report.other_window.label(),
                        report.whisper_dictate.label()
                    ));
                }
            }
        });
        ui.label("");
        ui.end_row();

        if start_requested {
            self.start_hotkey_verification(ui.ctx());
        }
        if fail_requested {
            if let Some(session) = self.hotkey_verification_session.as_mut() {
                session.fail_current();
                ui.ctx().request_repaint();
            }
        }
        if stop_requested {
            self.cancel_hotkey_verification("operator stopped guided test");
            self.settings_status =
                "Shortcut test stopped; partial results were kept for this session.".to_owned();
        }
    }

    pub(in crate::ui) fn start_hotkey_verification(&mut self, ctx: &egui::Context) {
        if self.runtime_state != RuntimeState::Stopped {
            self.settings_status = "Stop dictation before testing the shortcut.".to_owned();
            return;
        }
        if self.supervisor.is_teardown_pending() {
            self.settings_status =
                "Wait for runtime teardown to finish before testing the shortcut.".to_owned();
            return;
        }
        let planned_driver = match hotkey_capability(&self.settings.key) {
            HotkeyCapability::Invalid(err) => {
                self.settings_status = format!("Cannot test an invalid shortcut: {err:?}");
                return;
            }
            HotkeyCapability::Unsupported(reason) => {
                self.settings_status = format!("Cannot install this shortcut: {reason}");
                return;
            }
            HotkeyCapability::FallbackRisk { planned_driver, .. }
            | HotkeyCapability::Installable { planned_driver } => planned_driver,
        };
        if !crate::platform::foreground_window::focus_attribution_available() {
            self.settings_status =
                "Focus-aware shortcut verification is unavailable in this desktop session."
                    .to_owned();
            return;
        }
        self.hotkey_capture.cancel();
        self.cancel_hotkey_verification("new guided test requested");
        let ctx_for_repaint = ctx.clone();
        let repaint: crate::runtime::RepaintNotifier =
            std::sync::Arc::new(move || ctx_for_repaint.request_repaint());
        match HotkeyVerificationSession::start(&self.settings.key, &planned_driver, repaint) {
            Ok(session) => {
                let driver = session.report().driver.clone();
                self.hotkey_verification = None;
                self.hotkey_verification_session = Some(session);
                self.settings_status = format!(
                    "Shortcut diagnostic is starting with planned driver {driver}. Complete both focus tests after installation."
                );
            }
            Err(reason) => {
                crate::diag::log!(
                    "[hotkey/verify] diagnostic listener install failed chord={} reason={}",
                    self.settings.key.trim(),
                    reason
                );
                self.settings_status = format!(
                    "Could not install the shortcut diagnostic: {reason}. Try `pause` or another chord."
                );
            }
        }
    }
}

pub(super) fn guided_hotkey_verification_available(
    chord: &str,
    focus_attribution_available: bool,
) -> bool {
    focus_attribution_available
        && matches!(
            hotkey_capability(chord),
            HotkeyCapability::Installable { .. } | HotkeyCapability::FallbackRisk { .. }
        )
}
