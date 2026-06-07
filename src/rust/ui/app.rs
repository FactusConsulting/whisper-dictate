//! The `eframe::App` entry point: the per-frame `update` panel composition plus
//! the runtime lifecycle (start/stop/restart), worker-event polling, and the
//! runtime-log append primitives.

use super::*;
use crate::runtime::{default_worker_command, RuntimeEvent, WorkerCommand, WorkerEvent};

impl eframe::App for WhisperDictateApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_runtime();
        self.poll_background_task();
        let palette = ui_palette(&self.settings.ui_theme);
        apply_ui_theme(ctx, &self.settings.ui_text_scale, &self.settings.ui_theme);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        paint_sidebar_bridge(ctx, palette, &self.settings.ui_text_scale);

        egui::SidePanel::left("primary_navigation")
            .resizable(false)
            .show_separator_line(false)
            .exact_width(sidebar_width(&self.settings.ui_text_scale))
            .frame(
                egui::Frame::default()
                    .fill(palette.header_bg)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(14.0, 14.0)),
            )
            .show(ctx, |ui| self.sidebar(ui, palette));

        egui::TopBottomPanel::top("runtime_status")
            .resizable(false)
            .exact_height(top_status_bar_height(&self.settings.ui_text_scale))
            .frame(
                egui::Frame::default()
                    .fill(palette.panel_bg)
                    .stroke(egui::Stroke::new(0.8, palette.border_soft))
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| self.top_status_bar(ui, palette));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(palette.panel_bg)
                    .inner_margin(egui::Margin::symmetric(EDGE_MARGIN, EDGE_MARGIN)),
            )
            .show(ctx, |ui| match self.selected_tab {
                Tab::Log => self.runtime_tab(ui),
                Tab::Speech => self.settings_panel(ui, Self::core_tab),
                Tab::Quality => self.settings_panel(ui, Self::quality_tab),
                Tab::Dictionary => self.settings_panel(ui, Self::dictionary_tab),
                Tab::Output => self.settings_panel(ui, Self::output_tab),
                Tab::Post => self.settings_panel(ui, Self::post_processing_tab),
                Tab::Profiles => self.settings_panel(ui, Self::profiles_tab),
            });
    }
}

impl WhisperDictateApp {
    pub(in crate::ui) fn start_runtime(&mut self) {
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            return;
        }
        self.clear_audio_meter_and_device();
        let command = self.worker_command();
        self.append_runtime_log(format!("[ui] starting: {}", command.display()));
        if let Err(err) = self.supervisor.start(command) {
            self.append_runtime_log(format!("[ui] start failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn stop_runtime(&mut self) {
        self.clear_audio_meter();
        self.append_runtime_log("[ui] stopping runtime");
        if let Err(err) = self.supervisor.stop() {
            self.append_runtime_log(format!("[ui] stop failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn restart_runtime(&mut self) {
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            return;
        }
        let command = self.worker_command();
        self.clear_audio_meter_and_device();
        self.append_runtime_log(format!("[ui] restarting: {}", command.display()));
        if let Err(err) = self.supervisor.restart(command) {
            self.append_runtime_log(format!("[ui] restart failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn worker_command(&self) -> WorkerCommand {
        let mut command = default_worker_command();
        if let Some(xkb_layout) = effective_xkb_layout(&self.settings) {
            command.env.push((XKB_LAYOUT_ENV.to_owned(), xkb_layout));
        }
        if self.settings.stt_backend == "openai" {
            let key = self.stt_api_key_input.trim();
            if !key.is_empty() {
                command
                    .env
                    .push((STT_API_KEY_ENV.to_owned(), key.to_owned()));
            }
        }
        if matches!(self.settings.post_processor.as_str(), "openai" | "groq") {
            let post_key = self.post_api_key_input.trim();
            let key = if post_key.is_empty() {
                self.stt_api_key_input.trim()
            } else {
                post_key
            };
            if !key.is_empty() {
                command
                    .env
                    .push((POST_API_KEY_ENV.to_owned(), key.to_owned()));
            }
        }
        command
    }

    pub(in crate::ui) fn clear_audio_meter(&mut self) {
        self.audio_capture_opening = false;
        self.audio_capture_active = false;
        self.clear_audio_meter_readings();
    }

    /// Blank the live meter readings (level / dBFS / peak) without touching the
    /// capture-active flags — used whenever capture goes inactive so stale
    /// numbers don't linger on the gauge.
    fn clear_audio_meter_readings(&mut self) {
        self.audio_meter_level = 0.0;
        self.audio_meter_raw_dbfs = None;
        self.audio_meter_peak = None;
    }

    fn clear_audio_meter_and_device(&mut self) {
        self.clear_audio_meter();
        self.active_audio_device.clear();
    }

    fn ensure_stt_api_key_loaded_for_runtime(&mut self) {
        if self.settings.stt_backend != "openai" || !self.stt_api_key_input.trim().is_empty() {
            return;
        }
        self.reload_stt_api_key();
        if self.stt_api_key_input.trim().is_empty() {
            let provider = self.current_cloud_provider();
            let message = format!(
                "No {} API key loaded. Paste one in Speech and click Save API key before starting cloud STT.",
                provider.label()
            );
            self.stt_api_key_status = message.clone();
            self.append_runtime_log(format!("[ui] {message}"));
        }
    }

    pub(in crate::ui) fn cloud_stt_missing_api_key(&self) -> bool {
        self.settings.stt_backend == "openai" && self.stt_api_key_input.trim().is_empty()
    }

    fn poll_runtime(&mut self) {
        for event in self.supervisor.poll() {
            match event {
                RuntimeEvent::Started { command } => {
                    self.append_runtime_log(format!("[ui] started: {command}"));
                }
                RuntimeEvent::Worker(event) => {
                    if event.event == "status" {
                        self.update_worker_status(&event);
                        if let Some(line) = worker_status_log_line(&event) {
                            self.append_runtime_log(line);
                        }
                    } else if event.event == "audio" {
                        self.update_worker_audio(&event);
                    } else if event.event == "utterance" {
                        if let Some(line) = worker_utterance_log_line(&event) {
                            self.append_runtime_log(line);
                        }
                    }
                }
                RuntimeEvent::Stdout(line) | RuntimeEvent::Stderr(line) => {
                    self.append_runtime_log(line);
                }
                RuntimeEvent::Exited { code } => {
                    self.clear_audio_meter();
                    self.append_runtime_log(format!(
                        "[ui] runtime exited with code {}",
                        code.map_or_else(|| "unknown".to_owned(), |c| c.to_string())
                    ));
                }
                RuntimeEvent::Error(message) => {
                    self.clear_audio_meter();
                    self.append_runtime_log(format!("[ui] runtime error: {message}"));
                }
            }
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn update_worker_status(&mut self, event: &WorkerEvent) {
        if let Some(audio_device) = worker_event_string(&event.payload, "audio_device") {
            self.active_audio_device = audio_device;
        }
        if let Some(state) = event.state.as_deref() {
            self.audio_capture_opening = state == "opening";
            if let Some(active) = audio_capture_active_for_worker_state(state) {
                self.audio_capture_active = active;
                if !active {
                    self.clear_audio_meter_readings();
                }
            }
        }
    }

    pub(in crate::ui) fn update_worker_audio(&mut self, event: &WorkerEvent) {
        if let Some(audio_device) = worker_event_string(&event.payload, "audio_device") {
            self.active_audio_device = audio_device;
        }
        if let Some(level) = worker_event_f32(&event.payload, "level") {
            self.audio_meter_level = level.clamp(0.0, 1.0);
        }
        if let Some(raw_dbfs) = worker_event_f32(&event.payload, "raw_dbfs") {
            self.audio_meter_raw_dbfs = Some(raw_dbfs);
        }
        if let Some(peak) = worker_event_f32(&event.payload, "peak") {
            self.audio_meter_peak = Some(peak);
        }
        if let Some(state) = event.state.as_deref() {
            self.audio_capture_opening = state == "opening";
            if let Some(active) = audio_capture_active_for_worker_state(state) {
                self.audio_capture_active = active;
                if !active {
                    self.clear_audio_meter_readings();
                }
            }
        } else {
            self.audio_capture_active = true;
        }
    }

    pub(in crate::ui) fn append_runtime_log(&mut self, line: impl AsRef<str>) {
        if !self.runtime_log.is_empty() {
            self.runtime_log.push('\n');
        }
        self.runtime_log.push_str(line.as_ref());
        self.runtime_log_scroll_to_bottom = true;
    }

    pub(in crate::ui) fn append_runtime_output(&mut self, output: &str) {
        if output.is_empty() {
            return;
        }
        self.append_runtime_log(output);
    }
}
