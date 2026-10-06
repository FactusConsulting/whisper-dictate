//! The `eframe::App` entry point: the per-frame `update` panel composition plus
//! the runtime lifecycle (start/stop/restart), worker-event polling, and the
//! runtime-log append primitives.

mod model_policy;
mod polling;
mod runtime_lifecycle;
mod view;
mod worker_events;

use super::*;
use crate::runtime::{default_worker_command, RuntimeEvent, WorkerCommand, WorkerEvent};

/// Maximum size (bytes) kept in `runtime_log`. When exceeded, the oldest whole
/// lines are dropped until the log is under the cap, and a single marker line
/// is prepended so the user knows the history was trimmed.
pub(in crate::ui) const RUNTIME_LOG_MAX_CHARS: usize = 200_000;
const RUNTIME_LOG_TRIM_HEADROOM_CHARS: usize = 32_000;

pub(in crate::ui) const TRIM_MARKER: &str = "[ui] \u{2026}older log trimmed\u{2026}";
const ACTIVE_REPAINT_MS: u64 = 80;
const IDLE_REPAINT_MS: u64 = 1000;
pub(in crate::ui) const WHISPER_MODEL_PATH_ENV: &str = "VOICEPI_WHISPER_MODEL_PATH";

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

pub(in crate::ui) fn injection_viewport_mouse_passthrough(pipeline_stage: Option<&str>) -> bool {
    pipeline_stage == Some("injecting")
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
    // Leave enough headroom that a saturated long-running session does not
    // trim and rebuild bounded projections again on nearly every appended
    // line. Whole-line/marker semantics stay unchanged; only the batch size of
    // each oldest-history eviction grows.
    let target = RUNTIME_LOG_MAX_CHARS
        .saturating_sub(RUNTIME_LOG_TRIM_HEADROOM_CHARS)
        .saturating_sub(marker_overhead);

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

impl WhisperDictateApp {
    /// Drive lifecycle work that must continue while the native viewport is
    /// minimized, occluded, or otherwise hidden. eframe 0.36 calls `App::logic`
    /// without an egui paint pass in that state, so none of this may live only
    /// in `App::ui`.
    pub(in crate::ui) fn run_non_visual_logic(&mut self, ctx: &egui::Context) {
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
        self.cancel_hotkey_verification_if_controls_hidden();
        self.poll_hotkey_verification();
        self.poll_background_task();
        // Drive the corpus batch-record sequence: after one clip's done-event is
        // applied (in poll_background_task), launch the next item once the small
        // inter-clip gap elapses. Cheap no-op when no batch is active.
        self.poll_corpus_batch();
        self.ensure_audio_devices_loaded();
        self.poll_update_check();
        self.runtime_log_cache.sync_if_needed(&self.runtime_log);
        let mouse_passthrough = injection_viewport_mouse_passthrough(self.pipeline_stage);
        if mouse_passthrough != self.injection_viewport_mouse_passthrough {
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(mouse_passthrough));
            self.injection_viewport_mouse_passthrough = mouse_passthrough;
        }
        // Mirror the dictation state onto the system-tray icon (recolours only on
        // change) and handle a tray left-click → focus. Runs in both full and
        // compact modes so the tray stays correct regardless of window layout.
        self.sync_tray(ctx);
        ctx.request_repaint_after(repaint_interval_for_state(
            self.compact_mode,
            self.runtime_state,
            self.audio_capture_opening,
            self.audio_capture_active,
            self.background_task.is_some(),
            self.pipeline_stage.is_some(),
        ));
    }
}

/// Return true when the post key we would push equals the ambient
/// `VOICEPI_POST_API_KEY`.
/// Pure function -- takes only strings, avoids touching `std::env` from
/// the hot path so this is testable without racing on process env from
/// parallel-running tests. Empty / whitespace-only ambient values do
/// not qualify as ownership: an unset variable is not a caller
/// declaration, and `export VOICEPI_POST_API_KEY=` is a leftover.
#[cfg(test)]
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
            self.pipeline_stage,
        );
        // Diagnostic trace — logs ONLY on transitions, so the runtime log shows
        // every tray colour change with the four inputs that drove it. Lets us
        // tell "worker stopped emitting recording" from "Rust never saw it" the
        // next time the tray gets stuck on green. Cheap: one push to a String
        // per state change, not per frame.
        if self.last_logged_tray_state != Some(state) {
            self.append_runtime_log(format!(
                "[ui] tray sync state={state:?} worker_running={worker_running} \
                 audio_active={} audio_opening={} pipeline_stage={:?} last_status={:?}",
                self.audio_capture_active,
                self.audio_capture_opening,
                self.pipeline_stage,
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

    pub(in crate::ui) fn clear_audio_meter(&mut self) {
        self.audio_capture_opening = false;
        self.audio_capture_active = false;
        self.clear_audio_meter_readings();
    }

    /// Drop the live pipeline-progress card state (stage + growing preview text)
    /// and the last-seen worker status state string. Called whenever the worker
    /// is no longer running a dictation — on stop/restart and on Exited/Error
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

    pub(in crate::ui) fn ensure_stt_api_key_loaded_for_runtime(&mut self) {
        if self.settings.stt_backend != "openai" || !self.stt_api_key_input.trim().is_empty() {
            return;
        }
        self.reload_stt_api_key();
        if self.cloud_stt_missing_api_key() && self.stt_api_key_input.trim().is_empty() {
            let message = self.cloud_stt_missing_api_key_message();
            self.stt_api_key_status = message.clone();
            self.append_runtime_log(format!("[ui] {message}"));
        }
    }

    /// Auto-populate the Microphone picker once, early in the app's life, so the
    /// user sees their real input devices without first clicking "Refresh
    /// devices". Fires exactly once (guarded by `audio_devices_loaded`) and only
    /// when no other background task is running, mirroring the one-shot discipline
    /// used for the GPU probe. A failed worker run just leaves the list empty
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
        let loopback = crate::privacy::is_loopback_url(self.settings.stt_base_url.trim());
        let in_process = self.current_cloud_provider() == CloudProvider::Nemotron
            && crate::cloud_api::is_nemotron_in_process_endpoint(self.settings.stt_base_url.trim());
        self.settings.stt_backend == "openai"
            && !loopback
            && !in_process
            && self.stt_api_key_input.trim().is_empty()
    }

    pub(in crate::ui) fn cloud_stt_missing_api_key_message(&self) -> String {
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

    pub(in crate::ui) fn append_runtime_log(&mut self, line: impl AsRef<str>) {
        let line = line.as_ref();
        self.runtime_log_cache.sync_if_needed(&self.runtime_log);
        self.runtime_log_cache.append(line);
        if !self.runtime_log.is_empty() {
            self.runtime_log.push('\n');
        }
        self.runtime_log.push_str(line);
        let trimmed = self.runtime_log.len() > RUNTIME_LOG_MAX_CHARS;
        trim_runtime_log(&mut self.runtime_log);
        self.runtime_log_cache
            .finish_append(&self.runtime_log, trimmed);
        self.runtime_log_scroll_to_bottom = true;
    }

    pub(in crate::ui) fn append_runtime_output(&mut self, output: &str) {
        if output.is_empty() {
            return;
        }
        self.append_runtime_log(output);
    }
}

/// Decide whether a process-exit diagnosis should replace the visible error.
/// Supervisor errors already describe the same failed lifecycle and are more
/// useful than a generic exit code; worker and UI errors are unrelated stale
/// state and should be replaced.
pub(in crate::ui) fn should_replace_exit_error(error_from_runtime: bool) -> bool {
    !error_from_runtime
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
