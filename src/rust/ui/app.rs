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
const ACTIVE_REPAINT_MS: u64 = 80;
const IDLE_REPAINT_MS: u64 = 1000;
pub(in crate::ui) const WHISPER_MODEL_PATH_ENV: &str = "VOICEPI_WHISPER_MODEL_PATH";

enum ExternalWhisperModelPath {
    Unset,
    Valid,
    Invalid,
}

pub(in crate::ui) fn repaint_interval_for_state(
    compact_mode: bool,
    runtime_state: crate::runtime::RuntimeState,
    audio_capture_opening: bool,
    audio_capture_active: bool,
    background_task_running: bool,
    pipeline_active: bool,
) -> std::time::Duration {
    let active = compact_mode
        || runtime_state != crate::runtime::RuntimeState::Stopped
        || audio_capture_opening
        || audio_capture_active
        || background_task_running
        || pipeline_active;
    std::time::Duration::from_millis(if active {
        ACTIVE_REPAINT_MS
    } else {
        IDLE_REPAINT_MS
    })
}

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
    // egui 0.34 renamed the required `App` method from `update(&Context, ..)` to
    // `ui(&mut Ui, ..)`; the old `update` is now a deprecated default. The panels
    // are now shown *inside* the root `ui` via `show(ui, ..)` (was
    // `show(ctx, ..)`; egui 0.35 collapsed the short-lived `show_inside`
    // back into `show` for the `Ui` overload), and `SidePanel`/`TopBottomPanel`
    // are unified into `Panel` (`Panel::left`/`top`/`bottom`,
    // `exact_width`/`exact_height` → `exact_size`).
    // The `Context` (still needed for the tray, theme, repaint and the sidebar
    // bridge painter) is taken from `ui.ctx()`. Layout/visuals are unchanged.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Bind the cloned `Context` into a named owned local (not a reference to a
        // temporary) so the borrow split off `ui` is explicit and not reliant on
        // temporary lifetime extension; pass it as `&ctx` where a `&Context` is
        // needed.
        let ctx = ui.ctx().clone();
        // Install the runtime supervisor's repaint notifier on the first frame.
        // It wakes egui whenever a worker event arrives — without it, events
        // that land while the window has no foreground attention sit in the
        // mpsc channel until the next ~80 ms repaint tick, which on Windows
        // simply does not fire when nothing tells egui to redraw. The visible
        // symptom was a tray icon that stayed GREEN through a full PTT cycle
        // after ~10 min of idle: worker events fine, UI just not awake to
        // process them. has_repaint_notifier() makes this idempotent — only
        // the first frame actually installs it.
        if !self.supervisor.has_repaint_notifier() {
            let ctx_for_notifier = ctx.clone();
            self.supervisor
                .set_repaint_notifier(std::sync::Arc::new(move || {
                    ctx_for_notifier.request_repaint();
                }));
        }
        // Poll the worker + background tasks every frame BEFORE any mode branch so
        // dictation keeps flowing (and the meter/log keep updating) whether the UI
        // is in the full window or the compact strip.
        self.poll_runtime();
        self.poll_hotkey_capture(&ctx);
        self.poll_background_task();
        // Drive the corpus batch-record sequence: after one clip's done-event is
        // applied (in poll_background_task), launch the next item once the small
        // inter-clip gap elapses. Cheap no-op when no batch is active.
        self.poll_corpus_batch();
        self.ensure_audio_devices_loaded();
        self.poll_update_check();
        // Mirror the dictation state onto the system-tray icon (recolours only on
        // change) and handle a tray left-click → focus. Runs in both full and
        // compact modes so the tray stays correct regardless of window layout.
        self.sync_tray(&ctx);
        let palette = ui_palette(&self.settings.ui_theme);
        apply_ui_theme(&ctx, &self.settings.ui_text_scale, &self.settings.ui_theme);
        ctx.request_repaint_after(repaint_interval_for_state(
            self.compact_mode,
            self.runtime_state,
            self.audio_capture_opening,
            self.audio_capture_active,
            self.background_task.is_some(),
            self.pipeline_stage.is_some(),
        ));

        // Compact mode: a single tiny CentralPanel with one control row — no
        // sidebar, tabs, log, or message bars. The viewport is already resized /
        // raised always-on-top by `set_compact_mode`; here we only render.
        if self.compact_mode {
            egui::CentralPanel::default()
                .frame(egui::Frame::default().fill(palette.panel_bg).inner_margin(
                    egui::Margin::symmetric(EDGE_MARGIN as i8, EDGE_MARGIN as i8),
                ))
                .show(ui, |ui| self.compact_panel(ui, palette));
            return;
        }

        paint_sidebar_bridge(&ctx, palette, &self.settings.ui_text_scale);

        egui::Panel::left("primary_navigation")
            .resizable(false)
            .show_separator_line(false)
            .exact_size(sidebar_width(&self.settings.ui_text_scale))
            .frame(
                egui::Frame::default()
                    .fill(palette.header_bg)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(14, 14)),
            )
            .show(ui, |ui| self.sidebar(ui, palette));

        egui::Panel::top("runtime_status")
            .resizable(false)
            .exact_size(top_status_bar_height(&self.settings.ui_text_scale))
            .frame(
                egui::Frame::default()
                    .fill(palette.panel_bg)
                    .stroke(egui::Stroke::new(0.8, palette.border_soft))
                    .inner_margin(egui::Margin::symmetric(16, TOP_PANEL_V_MARGIN as i8)),
            )
            .show(ui, |ui| self.top_status_bar(ui, palette));

        // Thin global status bar: saved/unsaved state + the latest message,
        // on every tab, replacing the per-page Messages card.
        egui::Panel::bottom("status_message_bar")
            .resizable(false)
            .exact_size(bottom_message_bar_height(&self.settings.ui_text_scale))
            .frame(
                egui::Frame::default()
                    .fill(palette.header_bg)
                    .stroke(egui::Stroke::new(0.8, palette.border_soft))
                    // Match the central panel's left inset so the status dot lines
                    // up with the content above it.
                    .inner_margin(egui::Margin::symmetric(EDGE_MARGIN as i8, 4)),
            )
            .show(ui, |ui| self.status_message_bar(ui, palette));

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(palette.panel_bg).inner_margin(
                egui::Margin::symmetric(EDGE_MARGIN as i8, EDGE_MARGIN as i8),
            ))
            .show(ui, |ui| match self.selected_tab {
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

/// Return true when the post key we would push equals the ambient
/// `VOICEPI_POST_API_KEY` (Codex P2 round-4 `PRRT_kwDOSfNjQs6UZxN2`).
/// Pure function -- takes only strings, avoids touching `std::env` from
/// the hot path so this is testable without racing on process env from
/// parallel-running tests. Empty / whitespace-only ambient values do
/// not qualify as ownership: an unset variable is not a caller
/// declaration, and `export VOICEPI_POST_API_KEY=` is a leftover.
pub(in crate::ui) fn post_key_is_ambient_env_owned(
    pushed_key: &str,
    ambient_post_key: Option<&str>,
) -> bool {
    ambient_post_key
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some_and(|v| pushed_key == v)
}

impl WhisperDictateApp {
    fn external_whisper_model_path(&self) -> ExternalWhisperModelPath {
        match std::env::var(WHISPER_MODEL_PATH_ENV) {
            Err(std::env::VarError::NotPresent) => ExternalWhisperModelPath::Unset,
            Err(std::env::VarError::NotUnicode(_)) => ExternalWhisperModelPath::Invalid,
            Ok(path) if std::path::Path::new(path.trim()).is_file() => {
                ExternalWhisperModelPath::Valid
            }
            Ok(_) => ExternalWhisperModelPath::Invalid,
        }
    }

    pub(in crate::ui) fn has_external_whisper_model_path(&self) -> bool {
        matches!(
            self.external_whisper_model_path(),
            ExternalWhisperModelPath::Valid
        )
    }

    pub(in crate::ui) fn selected_whisper_model_warning(&self) -> Option<String> {
        self.whisper_model_warning(&self.settings.stt_backend, &self.settings.model)
    }

    fn runtime_whisper_model_warning(&self) -> Option<String> {
        let ambient_env = crate::runtime::in_process::ambient_session_env();
        let command = crate::runtime::default_worker_command_with_ambient_env(&ambient_env);
        let effective = |name: &str, fallback: &str| {
            command
                .env
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| fallback.to_owned())
        };
        let stt_backend = effective("VOICEPI_STT_BACKEND", &self.saved_settings.stt_backend);
        let model = effective("VOICEPI_MODEL", &self.saved_settings.model);
        self.whisper_model_warning(&stt_backend, &model)
    }

    fn whisper_model_warning(&self, stt_backend: &str, model: &str) -> Option<String> {
        if SttBackendMode::from_raw(stt_backend) != SttBackendMode::Whisper {
            return None;
        }

        match self.external_whisper_model_path() {
            ExternalWhisperModelPath::Valid => return None,
            ExternalWhisperModelPath::Invalid => {
                return Some(format!(
                    "{WHISPER_MODEL_PATH_ENV} must point to an existing GGML model file."
                ));
            }
            ExternalWhisperModelPath::Unset => {}
        }

        let Some(entry) = crate::whisper::model_manager::find(model) else {
            return Some(format!(
                "{} is not supported. Choose a listed model before recording.",
                model
            ));
        };
        let visible_in_picker = crate::whisper::model_manager::visible_catalog()
            .any(|candidate| candidate.name == entry.name);
        let local_only = self.local_only_downloads_blocked();
        let Some(availability) = self.whisper_model_downloads.cached_availability(entry) else {
            let availability = if crate::whisper::model_manager::is_downloaded(entry) {
                crate::ui::whisper_models_state::ModelAvailability::Available
            } else {
                crate::ui::whisper_models_state::ModelAvailability::Missing
            };
            return model_download_warning(model, availability, visible_in_picker, local_only);
        };
        model_download_warning(model, availability, visible_in_picker, local_only)
    }

    pub(in crate::ui) fn local_only_enabled(&self) -> bool {
        if self.supervisor.is_running_or_restarting() {
            return Self::local_only_env_enabled();
        }
        self.desired_local_only()
    }

    pub(in crate::ui) fn local_only_change_pending(&self) -> bool {
        self.supervisor.is_running_or_restarting()
            && Self::local_only_env_enabled() != self.desired_local_only()
    }

    fn desired_local_only(&self) -> bool {
        let ambient_env = crate::runtime::in_process::ambient_session_env();
        let ambient_override = ambient_env
            .get("VOICEPI_LOCAL_ONLY")
            .is_some_and(|value| matches!(value.trim(), "1" | "true" | "True" | "TRUE"));
        self.saved_settings.local_only || ambient_override
    }

    /// Return the privacy state that should govern new model transfers.
    ///
    /// A requested change is not active until the runtime restarts. When the
    /// user is turning local-only mode off, however, downloads must be
    /// available so a missing model can be repaired and the restart can
    /// complete; the running session remains unchanged until then.
    pub(in crate::ui) fn local_only_downloads_blocked(&self) -> bool {
        if self.local_only_change_pending() {
            return self.desired_local_only();
        }
        self.local_only_enabled()
    }

    fn local_only_env_enabled() -> bool {
        std::env::var("VOICEPI_LOCAL_ONLY")
            .ok()
            .is_some_and(|value| matches!(value.trim(), "1" | "true" | "True" | "TRUE"))
    }

    /// Recolour the system-tray icon to mirror the current dictation state and
    /// react to a tray left-click by focusing the main window. Purely additive:
    /// on non-Windows it is a no-op stub, and even on Windows a failed tray init
    /// is logged exactly once and then ignored — the dictation flow never depends
    /// on the tray.
    ///
    /// Uses the raw last worker status state string plus the audio capture flags.
    /// Stateless audio-meter events can leave the last status at `"ready"` while
    /// push-to-talk is already held, so the capture flags override the status
    /// fallback for the notification-area colour.
    fn sync_tray(&mut self, ctx: &egui::Context) {
        let worker_running = self.runtime_state != RuntimeState::Stopped;
        let state = tray_state_for_capture(
            &self.last_worker_status_state,
            worker_running,
            self.audio_capture_opening,
            self.audio_capture_active,
        );
        // Diagnostic trace — logs ONLY on transitions, so the runtime log shows
        // every tray colour change with the four inputs that drove it. Lets us
        // tell "worker stopped emitting recording" from "Rust never saw it" the
        // next time the tray gets stuck on green. Cheap: one push to a String
        // per state change, not per frame.
        if self.last_logged_tray_state != Some(state) {
            self.append_runtime_log(format!(
                "[ui] tray sync state={state:?} worker_running={worker_running} \
                 audio_active={} audio_opening={} last_status={:?}",
                self.audio_capture_active,
                self.audio_capture_opening,
                self.last_worker_status_state,
            ));
            self.last_logged_tray_state = Some(state);
        }
        if let Err(reason) = self.tray.sync(state, &self.settings.ui_language) {
            // First (and only) failure: log it, then permanently disable the tray
            // so we never retry-spam or block the app on a headless/denied tray.
            self.append_runtime_log(format!(
                "[ui] system-tray icon unavailable, continuing without it: {reason}"
            ));
            self.tray.disable();
            return;
        }
        if self.tray.poll_interaction().activate_window {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    pub(in crate::ui) fn start_runtime(&mut self) {
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            let message = self.cloud_stt_missing_api_key_message();
            self.settings_status = message.clone();
            self.last_runtime_error = Some(message.clone());
            self.append_runtime_log(format!("[ui] start blocked: {message}"));
            return;
        }
        if let Some(warning) = self.runtime_whisper_model_warning() {
            self.settings_status = warning.clone();
            self.last_runtime_error = Some(warning.clone());
            self.append_runtime_log(format!("[ui] start blocked: {warning}"));
            return;
        }
        self.worker_ready = false;
        self.last_injection_failed = false;
        self.last_runtime_error = None;
        self.active_target_title.clear();
        self.active_target_process.clear();
        self.active_target_id.clear();
        self.clear_audio_meter_and_device();
        let command = self.runtime_worker_command();
        self.append_runtime_log(format!("[ui] starting: {}", command.display()));
        if let Err(err) = self.supervisor.start(command) {
            self.last_runtime_error = Some(err.to_string());
            self.append_runtime_log(format!("[ui] start failed: {err}"));
        } else {
            self.worker_start_time = Some(std::time::Instant::now());
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn stop_runtime(&mut self) {
        self.worker_ready = false;
        self.last_injection_failed = false;
        self.last_runtime_error = None;
        self.active_target_title.clear();
        self.active_target_process.clear();
        self.active_target_id.clear();
        self.clear_audio_meter_and_device();
        self.clear_pipeline_progress();
        self.append_runtime_log("[ui] stopping runtime");
        if let Err(err) = self.supervisor.stop() {
            self.append_runtime_log(format!("[ui] stop failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    pub(in crate::ui) fn restart_runtime(&mut self) {
        self.restart_runtime_inner(false);
    }

    pub(in crate::ui) fn restart_runtime_after_credential_change(&mut self) {
        self.restart_runtime_inner(true);
    }

    fn restart_runtime_inner(&mut self, credential_restart: bool) {
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            let message = self.cloud_stt_missing_api_key_message();
            self.settings_status = message.clone();
            self.last_runtime_error = Some(message.clone());
            self.append_runtime_log(format!("[ui] restart blocked: {message}"));
            return;
        }
        if let Some(warning) = self.runtime_whisper_model_warning() {
            let status = if self.local_only_change_pending() {
                format!("Restart pending: local-only change is not active. {warning}")
            } else if credential_restart {
                format!("Restart pending: credential change is not active. {warning}")
            } else {
                warning.clone()
            };
            self.settings_status = status.clone();
            self.last_runtime_error = Some(status.clone());
            self.append_runtime_log(format!("[ui] restart blocked: {status}"));
            return;
        }
        let command = self.runtime_worker_command();
        self.worker_ready = false;
        self.last_injection_failed = false;
        self.last_runtime_error = None;
        self.active_target_title.clear();
        self.active_target_process.clear();
        self.active_target_id.clear();
        self.clear_audio_meter_and_device();
        self.clear_pipeline_progress();
        self.append_runtime_log(format!("[ui] restarting: {}", command.display()));
        if let Err(err) = self.supervisor.restart(command) {
            self.last_runtime_error = Some(err.to_string());
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
        // Track provenance of the pushed post key so
        // `stamp_post_api_key_endpoint_marker` can bind the marker to the
        // ENDPOINT the underlying credential was resolved for -- not the
        // configured post endpoint blindly. Codex P1 round-2 #1
        // (`PRRT_kwDOSfNjQs6UXpn-` cmt 3665199618): a Groq-STT + OpenAI-post
        // setup used to stamp the OpenAI marker for a mirrored Groq STT
        // key, which the revalidation check then APPROVED for OpenAI --
        // a cross-provider leak of the STT key.
        let mut post_key_provenance = crate::runtime::cloud_api_keys::PostKeyProvenance::None;
        if matches!(self.settings.post_processor.as_str(), "openai" | "groq") {
            let post_key = self.post_api_key_input.trim();
            let stt_key = self.stt_api_key_input.trim();
            // Codex P1 round-3 (`PRRT_kwDOSfNjQs6UZdNL` cmt 3665509647):
            // `load_post_api_key_state` (see `ui/api_keys.rs:204-209`)
            // populates `post_api_key_input` from the `VOICEPI_STT_API_KEY`
            // env fallback when no post-specific credential is saved. The
            // field is then NON-EMPTY, so the old "empty post -> SttMirror,
            // else PostSpecific" rule stamped the OpenAI post endpoint for a
            // key that was actually the Groq STT key -- another
            // cross-provider approval. The fix: treat provenance as SttMirror
            // whenever the post field's VALUE equals the STT field's value.
            // Reason it's safe both ways: if the user pasted the same key
            // into both fields intentionally, that key IS the STT key (they
            // configured it as such), so binding the marker to the STT
            // endpoint is the correct + strictest classification. If the
            // values genuinely differ, the post field holds a post-specific
            // key and PostSpecific applies.
            let (key, provenance) = if post_key.is_empty() {
                (
                    stt_key,
                    crate::runtime::cloud_api_keys::PostKeyProvenance::SttMirror,
                )
            } else if !stt_key.is_empty() && post_key == stt_key {
                (
                    post_key,
                    crate::runtime::cloud_api_keys::PostKeyProvenance::SttMirror,
                )
            } else {
                (
                    post_key,
                    crate::runtime::cloud_api_keys::PostKeyProvenance::PostSpecific,
                )
            };
            if !key.is_empty() {
                // Codex P2 round-4 (`PRRT_kwDOSfNjQs6UZxN2` cmt 3665701506):
                // if the value we are about to push equals the ambient
                // `VOICEPI_POST_API_KEY` (i.e. it was loaded from the parent
                // env via `load_post_api_key_state`'s env fallback -- see
                // `ui/api_keys.rs:204-209`), the child inherits it from the
                // parent env automatically (`std::process::Command::envs`
                // extends rather than clears). Skipping the push AND the
                // marker preserves the documented "explicit env keys own
                // their resolution" compatibility contract -- an
                // env-provided key intentionally travels marker-free, and
                // a launcher marker stamped for the current endpoint would
                // otherwise reject the user's own key after any live
                // provider/profile change, with a restart producing the
                // same rejection because the same env value would load and
                // stamp again.
                let ambient_post_key = std::env::var("VOICEPI_POST_API_KEY").ok();
                if post_key_is_ambient_env_owned(key, ambient_post_key.as_deref()) {
                    // Caller-owned via ambient env; leave command.env alone
                    // (child inherits) and keep provenance = None so the
                    // shim's ambient-ownership rule fires.
                } else {
                    command
                        .env
                        .push((POST_API_KEY_ENV.to_owned(), key.to_owned()));
                    post_key_provenance = provenance;
                }
            }
        }
        // Codex P1 #666 #1 (`PRRT_kwDOSfNjQs6UXpn-`): the UI Start button
        // built the worker command directly and never called
        // `attach_cloud_api_keys`, so `VOICEPI_POST_API_KEY_ENDPOINT` was
        // never stamped for the primary Windows tray path -- the exact
        // Groq-to-OpenAI/custom live-change leak from #642 remained
        // exploitable through the shipping default flow. This shim stamps
        // the marker with the same rules as `attach_cloud_api_keys`, so
        // `require_endpoint_matches_marker` in the worker enforces the
        // endpoint check regardless of which entry point launched it.
        crate::runtime::cloud_api_keys::stamp_post_api_key_endpoint_marker(
            &mut command,
            post_key_provenance,
            &self.settings.post_processor,
            &self.settings.post_base_url,
            &self.settings.stt_backend,
            &self.settings.stt_base_url,
        );
        command
    }

    pub(in crate::ui) fn runtime_worker_command(&self) -> WorkerCommand {
        // Command construction checks whether API keys are owned by the
        // ambient launcher environment. Remove values written by the previous
        // native session first so they cannot masquerade as caller-owned and
        // disappear from a restart command.
        crate::runtime::in_process::restore_session_scoped_env();
        if crate::diag::trace_enabled() {
            crate::diag::log!("[ui/trace] restored prior session environment before command build");
        }
        self.worker_command()
    }

    pub(in crate::ui) fn clear_audio_meter(&mut self) {
        self.audio_capture_opening = false;
        self.audio_capture_active = false;
        self.clear_audio_meter_readings();
    }

    /// Drop the live pipeline-progress card state (stage + growing preview text)
    /// and the last-seen worker status state string. Called whenever the worker
    /// is no longer running a dictation — on stop/restart and on Exited/Error —
    /// so the sidebar recording indicator, the `render_pipeline_progress` card,
    /// and the tray icon can't stick on a stale "recording" stage after the
    /// worker is gone. Clearing `last_worker_status_state` means the tray will
    /// use the empty-string fallback, which `tray_state_for` maps to Ready/grey
    /// depending on whether the worker is running — the correct behaviour once
    /// the runtime_state has also flipped to Stopped.
    pub(in crate::ui) fn clear_pipeline_progress(&mut self) {
        self.pipeline_stage = None;
        self.pipeline_preview = None;
        self.last_worker_status_state = String::new();
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
        // A fresh start/restart can't be showing a stale device-unusable banner
        // from the previous run — the worker will re-report on the next capture.
        self.device_error = None;
    }

    fn ensure_stt_api_key_loaded_for_runtime(&mut self) {
        if self.settings.stt_backend != "openai" || !self.stt_api_key_input.trim().is_empty() {
            return;
        }
        self.reload_stt_api_key();
        if self.stt_api_key_input.trim().is_empty() {
            let message = self.cloud_stt_missing_api_key_message();
            self.stt_api_key_status = message.clone();
            self.append_runtime_log(format!("[ui] {message}"));
        }
    }

    /// Auto-populate the Microphone picker once, early in the app's life, so the
    /// user sees their real input devices without first clicking "Refresh
    /// devices". Fires exactly once (guarded by `audio_devices_loaded`) and only
    /// when no other background task is running, mirroring the one-shot discipline
    /// used for the GPU probe. A failed worker run just leaves the list empty —
    /// the guard still flips, so we never spam, and the manual button stays as the
    /// re-scan path (e.g. after plugging in a mic).
    fn ensure_audio_devices_loaded(&mut self) {
        if self.audio_devices_loaded || self.background_task.is_some() {
            return;
        }
        self.audio_devices_loaded = true;
        self.run_list_audio_devices();
    }

    /// Periodic, non-blocking "update available" poll.
    ///
    /// PRIVACY: when it runs, the spawned thread only does an anonymous GET on
    /// the public GitHub Pages version feed and sends NO data anywhere.
    ///
    /// Behaviour:
    /// - When the `update_check` setting is OFF, or `local_only` is ON, the check
    ///   is skipped entirely and any stale `update_available` badge is cleared
    ///   (an in-flight poll is also abandoned so its late result is ignored).
    /// - Otherwise, if no poll is in flight and we've never checked OR the clamped
    ///   interval has elapsed (measured with `Instant`, not wall-clock), one — and
    ///   only one — background thread is spawned and `last_update_check` recorded.
    /// - A completed poll applies [`apply_update_outcome`]:
    ///   `Newer(v)` → badge shown; `UpToDate` → badge cleared;
    ///   `Failed` → badge untouched (transient network error must not wipe a
    ///   previously-found update). `last_update_check` is recorded in all cases so
    ///   the normal interval applies before the next retry.
    pub(in crate::ui) fn poll_update_check(&mut self) {
        // Adopt a finished poll's result first (whether or not we still want to
        // poll), then drop the receiver so the next cycle can start fresh.
        if let Some(outcome) = self
            .update_check_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.update_available = apply_update_outcome(self.update_available.take(), outcome);
            self.update_check_rx = None;
        }

        // Disabled or local-only: skip and clear any stale badge / in-flight poll.
        if !self.settings.update_check || self.settings.local_only {
            self.update_available = None;
            self.update_check_rx = None;
            return;
        }

        // Never run more than one check at a time.
        if self.update_check_rx.is_some() {
            return;
        }

        let interval = poll_interval(&self.settings.update_check_interval_minutes);
        let due = self
            .last_update_check
            .is_none_or(|last| last.elapsed() >= interval);
        if !due {
            return;
        }

        self.last_update_check = Some(std::time::Instant::now());
        // The "include release candidates" opt-in is read here, at poll time, so
        // it is LIVE: toggling it takes effect on the next scheduled poll without
        // a restart. When off, RCs in the feed are invisible (stable-only).
        self.update_check_rx = Some(spawn_update_check(
            self.app_version.clone(),
            self.settings.update_include_prereleases,
        ));
    }

    pub(in crate::ui) fn cloud_stt_missing_api_key(&self) -> bool {
        // A self-hosted (Custom) endpoint usually needs no key, so don't block
        // start on an empty key for it.
        self.settings.stt_backend == "openai"
            && self.current_cloud_provider() != CloudProvider::Custom
            && self.stt_api_key_input.trim().is_empty()
    }

    fn cloud_stt_missing_api_key_message(&self) -> String {
        format!(
            "No {} API key loaded. Paste one in Speech and click Save API key before starting cloud STT.",
            self.current_cloud_provider().label()
        )
    }

    /// Mirror the process-wide push-to-talk refusal into the banner field.
    ///
    /// Read every poll rather than latched on an event, because the slot IS
    /// the state: it is published when an install is refused and retracted
    /// the moment one succeeds, so mirroring it keeps the banner honest
    /// across restarts without a second lifecycle to get wrong.
    pub(in crate::ui) fn refresh_hotkey_conflict(&mut self) {
        self.hotkey_conflict = crate::hotkey::ptt_lock::report::current().map(|c| c.message());
    }

    fn poll_runtime(&mut self) {
        self.refresh_hotkey_conflict();
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
                    self.last_injection_failed = false;
                    self.clear_audio_meter();
                    self.clear_pipeline_progress();
                    self.device_error = None;
                    if code != Some(0) && self.last_runtime_error.is_none() {
                        self.last_runtime_error = Some(format!(
                            "runtime exited with code {}",
                            code.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
                        ));
                    }
                    self.append_runtime_log(format!(
                        "[ui] runtime exited with code {}",
                        code.map_or_else(|| "unknown".to_owned(), |c| c.to_string())
                    ));
                    self.handle_exit_crash_streak(code);
                }
                RuntimeEvent::Error(message) => {
                    self.worker_ready = false;
                    self.last_injection_failed = false;
                    self.clear_audio_meter();
                    self.clear_pipeline_progress();
                    self.device_error = None;
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

    fn poll_hotkey_capture(&mut self, ctx: &egui::Context) {
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

    pub(in crate::ui) fn handle_worker_event(&mut self, event: &WorkerEvent) {
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
            self.clear_pipeline_progress();
            self.last_transcript = event
                .payload
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    event
                        .payload
                        .get("text_preview")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(str::to_owned)
                });
            self.active_profile = event
                .payload
                .get("profile")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_owned);
            self.last_target_title = event
                .payload
                .get("target_title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            self.last_target_process = event
                .payload
                .get("target_process")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            self.last_target_id = event
                .payload
                .get("target_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            self.last_inject_mode = event
                .payload
                .get("inject_mode")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|mode| !mode.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    let mode = self.settings.inject_mode.trim();
                    (!mode.is_empty()).then(|| mode.to_owned())
                });
            self.last_runtime_error = event
                .payload
                .get("inject_error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            self.last_injection_failed = self.last_runtime_error.is_some();
            if let Some(line) = worker_utterance_log_line(event) {
                self.append_runtime_log(line);
            }
        }
    }

    pub(in crate::ui) fn update_worker_status(&mut self, event: &WorkerEvent) {
        // Surface (or clear) the prominent "device unusable" banner. The worker
        // emits `state="error"` with `reason="device_unusable"` and an actionable
        // `error` message naming the microphone the user picked that won't open on
        // any audio backend. Show it near the live-dictation header so the user
        // sees it without opening the Debug log; any later working device signal
        // (a recording/ready status carrying an audio_device) clears it.
        let is_device_unusable = event.state.as_deref() == Some("error")
            && worker_event_string(&event.payload, "reason").as_deref() == Some("device_unusable");
        if is_device_unusable {
            self.device_error = worker_event_string(&event.payload, "error").or_else(|| {
                worker_event_string(&event.payload, "audio_device")
                    .map(|device| format!("Microphone {device} could not be opened."))
            });
        }
        if let Some(audio_device) = worker_event_string(&event.payload, "audio_device") {
            // A working device was reported (anything other than the unusable
            // error event itself) → the previously-picked-but-broken mic is no
            // longer the active one, so drop the banner.
            if !is_device_unusable {
                self.device_error = None;
            }
            self.active_audio_device = audio_device;
        }
        if let Some(state) = event.state.as_deref() {
            if matches!(
                state,
                "opening" | "recording" | "transcribing" | "post-processing" | "injecting"
            ) {
                self.last_runtime_error = None;
                self.last_injection_failed = false;
            }
            if state == "profile" {
                self.active_profile = worker_event_string(&event.payload, "active_profile")
                    .filter(|profile| !profile.trim().is_empty());
                self.active_target_title =
                    worker_event_string(&event.payload, "target_title").unwrap_or_default();
                self.active_target_process =
                    worker_event_string(&event.payload, "target_process").unwrap_or_default();
                self.active_target_id =
                    worker_event_string(&event.payload, "target_id").unwrap_or_default();
            }
            match state {
                "ready" if !self.last_injection_failed => self.last_runtime_error = None,
                "error" | "failed" | "capture_lost" => {
                    self.last_injection_failed = false;
                    self.last_runtime_error = worker_event_string(&event.payload, "error")
                        .or_else(|| worker_event_string(&event.payload, "reason"))
                        .or_else(|| Some(state.replace('_', " ")));
                }
                _ => {}
            }
            // Store the raw state string for tray-icon mapping. "preview" is
            // display-only and carries no state change for the tray, so we
            // keep the previous state in that case (the mic is still recording).
            if state != "preview" {
                self.last_worker_status_state = state.to_owned();
            }
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
