//! polling responsibilities for the desktop controller.

use super::*;

impl WhisperDictateApp {
    pub(super) fn poll_runtime(&mut self) {
        self.refresh_hotkey_conflict();
        for event in self.supervisor.poll() {
            match event {
                RuntimeEvent::Started {
                    command,
                    hotkey_driver,
                    hotkey_chord,
                } => {
                    if let Some(settings) = self.pending_runtime_settings.take() {
                        self.applied_settings = settings;
                    }
                    self.installed_hotkey = Some(InstalledHotkeyStatus {
                        chord: hotkey_chord,
                        driver: hotkey_driver,
                    });
                    self.last_runtime_error_from_runtime = false;
                    self.append_runtime_log(format!("[ui] started: {command}"));
                }
                RuntimeEvent::Worker(event) => self.handle_worker_event(&event),
                RuntimeEvent::Stdout(line) | RuntimeEvent::Stderr(line) => {
                    self.append_runtime_log(line);
                }
                RuntimeEvent::Exited { code } => self.handle_runtime_exit(code),
                RuntimeEvent::Error(message) => {
                    self.pending_runtime_settings = None;
                    self.worker_ready = false;
                    self.last_injection_failed = false;
                    self.clear_audio_meter();
                    self.clear_pipeline_progress();
                    self.device_error = None;
                    self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
                    self.last_runtime_error_from_runtime = true;
                    self.last_runtime_error = Some(message.clone());
                    self.append_runtime_log(format!("[ui] runtime error: {message}"));
                }
            }
        }
        // Poll the background GPU probe and adopt the result once available.
        if let Some(result) = self.gpu_probe.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.gpu_total_mb = result;
            self.gpu_probe = None;
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn handle_runtime_exit(&mut self, code: Option<i32>) {
        self.worker_ready = false;
        self.last_injection_failed = false;
        self.clear_audio_meter();
        self.clear_pipeline_progress();
        self.device_error = None;
        self.installed_hotkey = None;
        if code != Some(0) {
            self.runtime_error_revision = self.runtime_error_revision.wrapping_add(1);
            let exit_message = format!(
                "runtime exited with code {}",
                code.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
            );
            if should_replace_exit_error(self.last_runtime_error_from_runtime) {
                self.last_runtime_error = Some(exit_message);
            }
            self.last_runtime_error_from_runtime = true;
        }
        self.append_runtime_log(format!(
            "[ui] runtime exited with code {}",
            code.map_or_else(|| "unknown".to_owned(), |c| c.to_string())
        ));
        self.handle_exit_crash_streak(code);
    }

    pub(in crate::ui) fn cancel_hotkey_verification_if_controls_hidden(&mut self) {
        if self.hotkey_verification_session.is_some()
            && (self.selected_tab != Tab::Speech || self.compact_mode)
        {
            self.cancel_hotkey_verification("guided-test controls hidden");
            self.settings_status =
                "Shortcut test stopped because its controls were hidden; partial results were kept."
                    .to_owned();
        }
    }

    pub(super) fn poll_hotkey_verification(&mut self) {
        let stale = self
            .hotkey_verification_session
            .as_ref()
            .is_some_and(|session| !session.report().belongs_to(&self.settings.key));
        if stale {
            self.cancel_hotkey_verification("configured chord changed during guided test");
            self.settings_status =
                "Shortcut test stopped because the chord changed; start a new test for the edited chord."
                    .to_owned();
            return;
        }
        let update = self
            .hotkey_verification_session
            .as_mut()
            .and_then(|session| {
                let changed = session.poll();
                changed.then(|| session.report().clone())
            });
        if crate::diag::debug_enabled() {
            if let Some(report) = update.as_ref() {
                crate::diag::log!(
                "[hotkey/verify/debug] focus results driver={} chord={} other_window={} whisper_focused={}",
                report.driver,
                report.chord,
                report.other_window.label(),
                report.whisper_dictate.label()
            );
            }
        }
        let failure = self
            .hotkey_verification_session
            .as_ref()
            .and_then(|session| session.failure_reason().map(str::to_owned));
        if let Some(reason) = failure {
            let session = self
                .hotkey_verification_session
                .take()
                .expect("failed verifier session is present");
            let report = session.report().clone();
            session.shutdown();
            self.settings_status =
                format!("Shortcut diagnostic stopped: {reason}. Try `pause` or another chord.");
            self.hotkey_verification = Some(report);
            return;
        }
        let complete = self
            .hotkey_verification_session
            .as_ref()
            .is_some_and(|session| session.report().is_complete());
        if complete {
            let session = self
                .hotkey_verification_session
                .take()
                .expect("completed session is present");
            let report = session.report().clone();
            session.shutdown();
            self.settings_status = if report.is_verified() {
                format!(
                    "Shortcut verified in both focus contexts with driver {}.",
                    report.driver
                )
            } else {
                "Shortcut failed in at least one focus context. Use `pause` or test another chord."
                    .to_owned()
            };
            self.hotkey_verification = Some(report);
        }
    }

    pub(in crate::ui) fn cancel_hotkey_verification(&mut self, reason: &str) {
        let Some(session) = self.hotkey_verification_session.take() else {
            return;
        };
        let report = session.report().clone();
        crate::diag::log!(
            "[hotkey/verify] diagnostic listener stopping reason={} driver={} chord={}",
            reason,
            report.driver,
            report.chord
        );
        session.shutdown();
        self.hotkey_verification = Some(report);
    }

    pub(super) fn poll_hotkey_capture(&mut self, ctx: &egui::Context) {
        if self.selected_tab != Tab::Speech {
            self.cancel_hotkey_capture_if_hidden();
            return;
        }
        if !self.hotkey_capture.is_listening() {
            return;
        }
        let events = ctx.input(|input| input.events.clone());
        self.poll_hotkey_capture_events(events);
    }

    pub(in crate::ui) fn poll_hotkey_capture_events(&mut self, events: Vec<egui::Event>) {
        for event in events {
            if matches!(event, egui::Event::WindowFocused(false)) {
                self.hotkey_capture.cancel();
                self.settings_status =
                    "Capture cancelled because the window lost focus.".to_owned();
                break;
            }
            let egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                ..
            } = event
            else {
                continue;
            };
            if repeat {
                continue;
            }
            let key = physical_key.unwrap_or(key);
            let Some(token) = capture_token_for_egui_key(key) else {
                if pressed {
                    self.hotkey_capture.reject();
                    self.settings_status =
                        "Capture cancelled: that key is not supported by the native listener."
                            .to_owned();
                }
                continue;
            };
            if let Some(chord) = self.hotkey_capture.observe(token, pressed) {
                self.settings_status =
                    format!("Captured {chord}. Confirm it in Speech > Hotkey before saving.");
                break;
            }
        }
    }

    pub(in crate::ui) fn cancel_hotkey_capture_if_hidden(&mut self) {
        if self.selected_tab != Tab::Speech && self.hotkey_capture.is_listening() {
            self.hotkey_capture.cancel();
            self.settings_status =
                "Shortcut capture cancelled because the Speech tab is no longer active.".to_owned();
        }
    }

    /// Track fast-crash streaks: when a non-clean exit happens within 10 s of
    /// start, increment the counter.  After 3 consecutive fast crashes, append
    /// an actionable advice line (once per streak).
    pub(in crate::ui) fn handle_exit_crash_streak(&mut self, code: Option<i32>) {
        let elapsed = self
            .worker_start_time
            .take()
            .map(|t| t.elapsed())
            .unwrap_or(std::time::Duration::MAX);

        if code == Some(0) || elapsed >= std::time::Duration::from_secs(10) {
            // Clean or long-lived exit — reset the streak.
            self.fast_crash_count = 0;
            return;
        }

        self.fast_crash_count += 1;

        if let Some(msg) = crash_streak_advice(self.fast_crash_count) {
            self.append_runtime_log(msg);
        }
    }
}
