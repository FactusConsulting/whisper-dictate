//! The `eframe::App` entry point: the per-frame `update` panel composition plus
//! the runtime lifecycle (start/stop/restart), worker-event polling, and the
//! runtime-log append primitives.

use super::*;
use crate::runtime::{default_worker_command, RuntimeEvent, WorkerCommand, WorkerEvent};

/// Maximum size (bytes) kept in `runtime_log`. When exceeded, the oldest whole
/// lines are dropped until the log is under the cap, and a single marker line
/// is prepended so the user knows the history was trimmed.
pub(in crate::ui) const RUNTIME_LOG_MAX_CHARS: usize = 200_000;

pub(in crate::ui) const TRIM_MARKER: &str = "[ui] \u{2026}older log trimmed\u{2026}";

/// Drop the oldest whole lines from `log` until its length is under
/// [`RUNTIME_LOG_MAX_CHARS`].  A single marker line is prepended to signal the
/// truncation; if one is already at the top it is not duplicated.
pub(in crate::ui) fn trim_runtime_log(log: &mut String) {
    if log.len() <= RUNTIME_LOG_MAX_CHARS {
        return;
    }

    // Reserve headroom for the marker + newline we will prepend.
    let marker_overhead = TRIM_MARKER.len() + 1;
    let target = RUNTIME_LOG_MAX_CHARS.saturating_sub(marker_overhead);

    // Drop whole lines from the front until the body fits within `target`.
    loop {
        if log.len() <= target {
            break;
        }
        // How many bytes we need to remove.
        let excess = log.len().saturating_sub(target);
        // Align to a UTF-8 character boundary.
        let excess_aligned = (excess..=log.len())
            .find(|&i| log.is_char_boundary(i))
            .unwrap_or(log.len());
        // Advance to the end of the current line (past the next '\n').
        match log[excess_aligned..].find('\n') {
            Some(nl) => {
                log.drain(..excess_aligned + nl + 1);
            }
            None => {
                // No newline: the entire remaining content is one long line.
                log.clear();
                break;
            }
        }
    }

    // Prepend the marker exactly once.
    if !log.starts_with(TRIM_MARKER) {
        let tail = log.clone();
        log.clear();
        log.push_str(TRIM_MARKER);
        if !tail.is_empty() {
            log.push('\n');
            log.push_str(&tail);
        }
    }
}

impl eframe::App for WhisperDictateApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll the worker + background tasks every frame BEFORE any mode branch so
        // dictation keeps flowing (and the meter/log keep updating) whether the UI
        // is in the full window or the compact strip.
        self.poll_runtime();
        self.poll_background_task();
        let palette = ui_palette(&self.settings.ui_theme);
        apply_ui_theme(ctx, &self.settings.ui_text_scale, &self.settings.ui_theme);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));

        // Compact mode: a single tiny CentralPanel with one control row — no
        // sidebar, tabs, log, or message bars. The viewport is already resized /
        // raised always-on-top by `set_compact_mode`; here we only render.
        if self.compact_mode {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::default()
                        .fill(palette.panel_bg)
                        .inner_margin(egui::Margin::symmetric(EDGE_MARGIN, EDGE_MARGIN)),
                )
                .show(ctx, |ui| self.compact_panel(ui, palette));
            return;
        }

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
                    .inner_margin(egui::Margin::symmetric(16.0, TOP_PANEL_V_MARGIN)),
            )
            .show(ctx, |ui| self.top_status_bar(ui, palette));

        // Thin global status bar: saved/unsaved state + the latest message,
        // on every tab, replacing the per-page Messages card.
        egui::TopBottomPanel::bottom("status_message_bar")
            .resizable(false)
            .exact_height(bottom_message_bar_height(&self.settings.ui_text_scale))
            .frame(
                egui::Frame::default()
                    .fill(palette.header_bg)
                    .stroke(egui::Stroke::new(0.8, palette.border_soft))
                    // Match the central panel's left inset so the status dot lines
                    // up with the content above it.
                    .inner_margin(egui::Margin::symmetric(EDGE_MARGIN, 4.0)),
            )
            .show(ctx, |ui| self.status_message_bar(ui, palette));

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
                Tab::System => self.settings_panel(ui, Self::system_tab),
            });
    }
}

impl WhisperDictateApp {
    pub(in crate::ui) fn start_runtime(&mut self) {
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            return;
        }
        self.worker_ready = false;
        self.clear_audio_meter_and_device();
        let command = self.worker_command();
        self.append_runtime_log(format!("[ui] starting: {}", command.display()));
        if let Err(err) = self.supervisor.start(command) {
            self.append_runtime_log(format!("[ui] start failed: {err}"));
        } else {
            self.worker_start_time = Some(std::time::Instant::now());
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn stop_runtime(&mut self) {
        self.worker_ready = false;
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
        self.worker_ready = false;
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
        // A self-hosted (Custom) endpoint usually needs no key, so don't block
        // start on an empty key for it.
        self.settings.stt_backend == "openai"
            && self.current_cloud_provider() != CloudProvider::Custom
            && self.stt_api_key_input.trim().is_empty()
    }

    fn poll_runtime(&mut self) {
        for event in self.supervisor.poll() {
            match event {
                RuntimeEvent::Started { command } => {
                    self.append_runtime_log(format!("[ui] started: {command}"));
                }
                RuntimeEvent::Worker(event) => self.handle_worker_event(&event),
                RuntimeEvent::Stdout(line) | RuntimeEvent::Stderr(line) => {
                    self.append_runtime_log(line);
                }
                RuntimeEvent::Exited { code } => {
                    self.worker_ready = false;
                    self.clear_audio_meter();
                    self.append_runtime_log(format!(
                        "[ui] runtime exited with code {}",
                        code.map_or_else(|| "unknown".to_owned(), |c| c.to_string())
                    ));
                    self.handle_exit_crash_streak(code);
                }
                RuntimeEvent::Error(message) => {
                    self.worker_ready = false;
                    self.clear_audio_meter();
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

    fn handle_worker_event(&mut self, event: &WorkerEvent) {
        if event.event == "status" {
            self.update_worker_status(event);
            if let Some(line) = worker_status_log_line(event) {
                self.append_runtime_log(line);
            }
        } else if event.event == "audio" {
            self.update_worker_audio(event);
        } else if event.event == "utterance" {
            // The dictation finished and settles into a Final card — clear the
            // live pipeline-progress card and its growing preview text.
            self.pipeline_stage = None;
            self.pipeline_preview = None;
            if let Some(line) = worker_utterance_log_line(event) {
                self.append_runtime_log(line);
            }
        }
    }

    pub(in crate::ui) fn update_worker_status(&mut self, event: &WorkerEvent) {
        if let Some(audio_device) = worker_event_string(&event.payload, "audio_device") {
            self.active_audio_device = audio_device;
        }
        if let Some(state) = event.state.as_deref() {
            self.audio_capture_opening = state == "opening";
            // "preview" is a mid-recording, display-only signal: it must NOT
            // overwrite pipeline_stage (which would clear the live "recording"
            // spinner), so special-case it before the stage assignment and just
            // capture the growing preview text. Every other known state updates
            // the stage as before; a stage that is no longer "recording" drops
            // the stale preview text.
            if state == "preview" {
                self.pipeline_preview =
                    worker_event_string(&event.payload, "text_preview").filter(|t| !t.is_empty());
            } else {
                self.pipeline_stage = pipeline_stage_for_worker_state(state);
                if self.pipeline_stage != Some("recording") {
                    self.pipeline_preview = None;
                }
            }
            if let Some(ready) = worker_ready_for_state(state) {
                if ready {
                    // Worker successfully loaded and is ready — clear any crash streak.
                    self.fast_crash_count = 0;
                }
                self.worker_ready = ready;
            }
            if let Some(active) = audio_capture_active_for_worker_state(state) {
                self.audio_capture_active = active;
                if !active {
                    self.clear_audio_meter_readings();
                }
            }
        }
    }

    /// The runtime state to show in the UI: the OS process spawns almost
    /// instantly (`Running`), but a local model can take a while to load, so we
    /// keep displaying `Starting` until the worker reports it is ready to receive
    /// speech. Control logic (Start/Stop enabling, audio meter) still uses the
    /// raw `runtime_state`.
    pub(in crate::ui) fn display_runtime_state(&self) -> RuntimeState {
        match self.runtime_state {
            RuntimeState::Running if !self.worker_ready => RuntimeState::Starting,
            other => other,
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
            // No state field: treat as active only when the worker is actually
            // running so a stale/malformed audio event can't linger the meter
            // after capture has stopped.
            if self.runtime_state == RuntimeState::Running {
                self.audio_capture_active = true;
            }
        }
    }

    pub(in crate::ui) fn append_runtime_log(&mut self, line: impl AsRef<str>) {
        if !self.runtime_log.is_empty() {
            self.runtime_log.push('\n');
        }
        self.runtime_log.push_str(line.as_ref());
        trim_runtime_log(&mut self.runtime_log);
        self.runtime_log_scroll_to_bottom = true;
    }

    pub(in crate::ui) fn append_runtime_output(&mut self, output: &str) {
        if output.is_empty() {
            return;
        }
        self.append_runtime_log(output);
    }
}

/// Returns an advice message when the crash streak count reaches the threshold
/// (exactly 3), and `None` otherwise.  Pure function — easy to unit-test
/// without constructing the full app.
///
/// `count` is the number of consecutive fast exits (<10 s, non-zero code).
pub(in crate::ui) fn crash_streak_advice(count: u32) -> Option<String> {
    if count == 3 {
        Some(
            "[ui] worker crashed 3 times in a row right after start — \
run Doctor (sidebar) and check the output above for the cause"
                .to_owned(),
        )
    } else {
        None
    }
}
