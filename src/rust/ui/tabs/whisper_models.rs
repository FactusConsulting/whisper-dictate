//! Settings tab section: download / verify Whisper GGML models.
//!
//! Wave 7-B (#348). The section sits inside the Speech tab's Whisper scope
//! group and offers one row per catalog entry from
//! `crate::whisper::model_manager::CATALOG`: name + size + description on
//! the left, a status badge + Download/Retry button on the right, and a
//! progress bar while a download is running.
//!
//! Backwards compat: `VOICEPI_WHISPER_MODEL_PATH` still wins for the runtime
//! inference path. This section only manages files in the curated
//! `whisper-models/` cache directory; a power user pointing the env var at
//! a custom file is unaffected.

use super::super::*;
use crate::ui::app::WHISPER_MODEL_PATH_ENV;
use crate::whisper::model_manager::{self, ModelEntry};
use crate::whisper::models_cli::human_bytes;

impl WhisperDictateApp {
    /// Keep a clean installation actionable from the first screen: local
    /// dictation cannot start until the selected GGML model has been verified,
    /// so show the same download control used in Settings directly above the
    /// runtime log. Existing custom model paths and cloud backends need no
    /// setup banner.
    pub(in crate::ui) fn selected_whisper_model_setup_banner(
        &mut self,
        ui: &mut egui::Ui,
        palette: UiPalette,
    ) {
        if SttBackendMode::from_raw(&self.settings.stt_backend) != SttBackendMode::Whisper
            || self.has_external_whisper_model_path()
        {
            return;
        }

        let model = self.settings.model.trim();
        let entry = model_manager::find(model);
        let visible_entry = entry.filter(|selected| {
            model_manager::visible_catalog().any(|candidate| candidate.name == selected.name)
        });
        let external_path_is_set = std::env::var_os(WHISPER_MODEL_PATH_ENV).is_some();
        let availability =
            visible_entry.map(|selected| self.whisper_model_downloads.availability_fast(selected));
        if availability == Some(crate::ui::whisper_models_state::ModelAvailability::Available) {
            return;
        }

        let message = if external_path_is_set {
            format!(
                "{WHISPER_MODEL_PATH_ENV} does not point to an existing GGML model file. Fix or remove it before recording."
            )
        } else if entry.is_none() {
            format!("{model} is not supported. Choose a listed model before recording.")
        } else if visible_entry.is_none() {
            format!(
                "{model} is a retained legacy model. Install it with `wd models download {model}`, or choose a current model."
            )
        } else if availability == Some(crate::ui::whisper_models_state::ModelAvailability::Checking)
        {
            format!("Verifying {model}. Recording stays disabled until verification completes.")
        } else {
            format!("Download {model} before starting local dictation.")
        };

        ui.add_space(8.0);
        egui::Frame::default()
            .fill(palette.surface_active_bg)
            .stroke(egui::Stroke::new(1.0, palette.warn_text))
            .corner_radius(egui::CornerRadius::same(PANEL_RADIUS))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(
                    egui::RichText::new("Local Whisper model required")
                        .strong()
                        .color(palette.warn_text),
                );
                ui.label(message);
                ui.add_space(4.0);
                if !external_path_is_set {
                    if let Some(selected) = visible_entry {
                        let any_running = self.whisper_model_downloads.any_in_progress();
                        let downloads_blocked = self.local_only_downloads_blocked();
                        self.render_whisper_model_row(ui, selected, any_running, downloads_blocked);
                        return;
                    }
                }
                if ui.button("Open Speech settings").clicked() {
                    self.selected_tab = Tab::Speech;
                }
            });
    }

    /// Render the "Whisper model download" section inside the Speech tab's
    /// Whisper scope group. Designed to be cheap to call every frame —
    /// the only state mutation paths are click handlers + the shared
    /// `WhisperModelDownloads` snapshot.
    pub(in crate::ui) fn whisper_model_download_section(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Whisper model downloads").strong());
        ui.label(
            egui::RichText::new(
                "Download a curated whisper.cpp GGML model into the user cache. \
                 The path the runtime uses is still VOICEPI_WHISPER_MODEL_PATH \
                 when that env var is set.",
            )
            .small()
            .weak(),
        );
        ui.add_space(4.0);
        let any_running = self.whisper_model_downloads.any_in_progress();
        let local_only = self.local_only_enabled();
        let local_only_pending = self.local_only_change_pending();
        let downloads_blocked = self.local_only_downloads_blocked();
        if local_only_pending {
            let message = if downloads_blocked {
                "Local-only mode is pending a successful runtime restart; downloads remain disabled until it is applied."
            } else {
                "Local-only mode is still active in the running runtime. Downloads are available because the saved change disables it; restart before recording."
            };
            ui.label(
                egui::RichText::new(message)
                    .small()
                    .color(ui.visuals().warn_fg_color),
            );
            ui.add_space(4.0);
        } else if local_only {
            ui.label(
                egui::RichText::new(
                    "Local-only mode is enabled; model downloads are disabled. Install models manually or disable local-only mode.",
                )
                .small()
                .color(ui.visuals().warn_fg_color),
            );
            ui.add_space(4.0);
        }
        // `visible_catalog()` (not `CATALOG`) so hidden test fixtures — the
        // tiny.en model CI downloads — never show up as a user choice.
        for entry in model_manager::visible_catalog() {
            self.render_whisper_model_row(ui, entry, any_running, downloads_blocked);
            ui.add_space(2.0);
        }
        if let Ok(dir) = model_manager::models_cache_dir() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("Cache: {}", dir.display()))
                    .small()
                    .weak(),
            );
        }
    }

    fn render_whisper_model_row(
        &mut self,
        ui: &mut egui::Ui,
        entry: &'static ModelEntry,
        any_running: bool,
        local_only: bool,
    ) {
        let job = self.whisper_model_downloads.job(entry.name);
        if let Some(status) =
            cancelled_download_completion_status(&self.settings_status, entry.name, job.as_ref())
        {
            self.settings_status = status;
        }
        let in_progress = matches!(
            job.as_ref().map(|j| &j.status),
            Some(crate::ui::whisper_models_state::DownloadStatus::InProgress),
        );
        // P1: use the metadata-keyed verify cache so we never SHA-256 a
        // multi-hundred-MB model file on the UI thread during a repaint.
        let already_cached = self.whisper_model_downloads.is_verified_fast(entry);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(entry.name).strong().monospace());
            ui.label(format!(
                "  {}  {}",
                human_bytes(entry.size_bytes),
                entry.description
            ));
        });
        ui.horizontal(|ui| {
            let (status_text, status_color) =
                whisper_model_status_label(already_cached, job.as_ref(), ui.visuals().text_color());
            let status_resp = ui.colored_label(status_color, status_text);
            // P3: surface the stored failure reason on hover so users can
            // distinguish a retryable network error from a SHA-256 mismatch.
            if let Some(crate::ui::whisper_models_state::DownloadJob {
                status: crate::ui::whisper_models_state::DownloadStatus::Failed(ref msg),
                ..
            }) = job.as_ref()
            {
                status_resp.on_hover_text(msg.as_str());
            }
            if in_progress {
                if ui
                    .button("Cancel")
                    .on_hover_text("Stop this download and remove its partial file.")
                    .clicked()
                    && self.whisper_model_downloads.cancel(entry.name)
                {
                    self.settings_status = format!("Cancelling Whisper model {}…", entry.name);
                }
            } else {
                // Disable a new download while another model is in progress.
                let button_label = if already_cached {
                    "Redownload"
                } else if matches!(
                    job.as_ref().map(|j| &j.status),
                    Some(crate::ui::whisper_models_state::DownloadStatus::Failed(_)),
                ) {
                    "Retry"
                } else {
                    "Download"
                };
                let can_start = self.whisper_model_downloads.can_start(entry.name);
                let disabled_reason =
                    whisper_download_disabled_reason(local_only, any_running, can_start);
                let tooltip = disabled_reason.map_or_else(
                    || {
                        format!(
                            "Download {} from {} to the user cache and verify its SHA-256.",
                            entry.name, entry.url
                        )
                    },
                    str::to_owned,
                );
                if ui
                    .add_enabled(disabled_reason.is_none(), egui::Button::new(button_label))
                    .on_hover_text(tooltip)
                    .clicked()
                {
                    let started = crate::ui::whisper_models_state::spawn_download(
                        &self.whisper_model_downloads,
                        entry.name,
                    );
                    if started {
                        self.settings_status = format!("Downloading Whisper model {}…", entry.name);
                    }
                }
            }
        });
        if let Some(job) = &job {
            match &job.status {
                crate::ui::whisper_models_state::DownloadStatus::InProgress => {
                    let fraction = job.fraction();
                    ui.horizontal(|ui| match fraction {
                        Some(f) => {
                            ui.add(
                                egui::ProgressBar::new(f)
                                    .desired_width(220.0)
                                    .show_percentage(),
                            );
                            ui.label(format!(
                                "{} / {}",
                                human_bytes(job.downloaded),
                                human_bytes(job.total.unwrap_or(job.downloaded)),
                            ));
                        }
                        None => {
                            ui.add(egui::Spinner::new());
                            ui.label(format!("{} downloaded", human_bytes(job.downloaded)));
                        }
                    });
                    // A transfer that has gone quiet looks identical to a
                    // healthy one until the engine kills it two minutes later
                    // (#574). Say so while there is still time for it to
                    // recover, and say for how long -- "no data" on its own
                    // reads as broken, which it is not yet.
                    if let crate::ui::whisper_models_state::Liveness::Slow(idle) = job.liveness() {
                        ui.label(
                            egui::RichText::new(format!(
                                "no data for {}s - still waiting",
                                idle.as_secs()
                            ))
                            .small()
                            .color(ui.visuals().warn_fg_color),
                        );
                    }
                }
                // P2: show the cached path so users can copy it or confirm
                // which file the transcription backend will pick up.
                crate::ui::whisper_models_state::DownloadStatus::Done(path) => {
                    ui.label(
                        egui::RichText::new(path.display().to_string())
                            .small()
                            .weak()
                            .monospace(),
                    );
                }
                crate::ui::whisper_models_state::DownloadStatus::Cancelled => {
                    ui.label(egui::RichText::new("download cancelled").small().weak());
                }
                // A stalled download and a broken connection point at
                // different remedies, so they must not render identically.
                // The hover text carries the full message either way; this is
                // the line the user reads WITHOUT hovering.
                crate::ui::whisper_models_state::DownloadStatus::Failed(msg) => {
                    if crate::ui::whisper_models_state::is_stall_failure(msg) {
                        ui.label(
                            egui::RichText::new(
                                "the server stopped sending data - press Retry, or raise \
                                 VOICEPI_MODEL_DOWNLOAD_IDLE_TIMEOUT_SECS on a slow link",
                            )
                            .small()
                            .color(ui.visuals().warn_fg_color),
                        );
                    }
                }
            }
        }
    }
}

fn whisper_download_disabled_reason(
    local_only: bool,
    any_running: bool,
    can_start: bool,
) -> Option<&'static str> {
    if local_only {
        Some("Downloads are disabled in local-only mode. Install the model manually or disable local-only mode.")
    } else if any_running {
        Some("Wait for the active model download to finish.")
    } else if !can_start {
        Some("Waiting for the cancelled download to release its network connection.")
    } else {
        None
    }
}

fn cancelled_download_completion_status(
    current_status: &str,
    model: &str,
    job: Option<&crate::ui::whisper_models_state::DownloadJob>,
) -> Option<String> {
    let cancelling = format!("Cancelling Whisper model {model}…");
    if current_status == cancelling
        && matches!(
            job.map(|job| &job.status),
            Some(crate::ui::whisper_models_state::DownloadStatus::Cancelled)
        )
    {
        Some(format!("Whisper model {model} download cancelled."))
    } else {
        None
    }
}

/// Pure status-label resolver: decide what badge to show next to a catalog
/// entry given (a) whether the file is already cached + verified on disk
/// and (b) the most recent download job (if any). Returns a (text, colour)
/// tuple so the render code can hand it straight to `ui.colored_label`.
/// Extracted into a free function so the precedence rules are unit-testable
/// without an egui context.
pub(in crate::ui) fn whisper_model_status_label(
    already_cached: bool,
    job: Option<&crate::ui::whisper_models_state::DownloadJob>,
    default_color: egui::Color32,
) -> (&'static str, egui::Color32) {
    use crate::ui::whisper_models_state::DownloadStatus;
    // The just-finished failure wins over the cached badge so a redownload
    // attempt that produced a SHA-256 mismatch can't quietly read as "OK"
    // because a previous valid copy is still on disk.
    if let Some(job) = job {
        match &job.status {
            DownloadStatus::Failed(_) => {
                return ("Failed", egui::Color32::from_rgb(220, 80, 80));
            }
            DownloadStatus::Done(_) => {
                return ("Downloaded", egui::Color32::from_rgb(80, 200, 120));
            }
            DownloadStatus::InProgress => {
                return ("Downloading", egui::Color32::from_rgb(220, 180, 80));
            }
            DownloadStatus::Cancelled => {
                if already_cached {
                    return ("Downloaded", egui::Color32::from_rgb(80, 200, 120));
                }
                return ("Cancelled", egui::Color32::from_rgb(220, 180, 80));
            }
        }
    }
    if already_cached {
        ("Downloaded", egui::Color32::from_rgb(80, 200, 120))
    } else {
        ("Missing", default_color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::whisper_models_state::{DownloadJob, DownloadStatus};
    use std::path::PathBuf;

    fn job(status: DownloadStatus) -> DownloadJob {
        DownloadJob {
            status,
            downloaded: 0,
            total: None,
            last_advance: std::time::Instant::now(),
        }
    }

    #[test]
    fn cancelled_redownload_keeps_the_downloaded_badge() {
        let default = egui::Color32::WHITE;
        assert_eq!(
            whisper_model_status_label(true, Some(&job(DownloadStatus::Cancelled)), default).0,
            "Downloaded"
        );
        assert_eq!(
            whisper_model_status_label(false, Some(&job(DownloadStatus::Cancelled)), default).0,
            "Cancelled"
        );
    }

    #[test]
    fn cancelled_download_replaces_the_cancelling_status() {
        let job = job(DownloadStatus::Cancelled);
        assert_eq!(
            cancelled_download_completion_status(
                "Cancelling Whisper model large-v3…",
                "large-v3",
                Some(&job),
            ),
            Some("Whisper model large-v3 download cancelled.".to_owned())
        );
        assert_eq!(
            cancelled_download_completion_status(
                "Downloading Whisper model large-v3…",
                "large-v3",
                Some(&job),
            ),
            None
        );
    }

    #[test]
    fn a_transfer_that_has_gone_quiet_is_reported_slow() {
        use crate::ui::whisper_models_state::{Liveness, SLOW_AFTER};
        let mut j = job(DownloadStatus::InProgress);

        // Just advanced: healthy, even though nothing is arriving right now.
        assert_eq!(j.liveness_at(j.last_advance), Liveness::Moving);

        // One tick short of the threshold is still healthy -- a multi-GB
        // download pausing briefly must not be reported as a problem.
        let almost = j.last_advance + SLOW_AFTER - std::time::Duration::from_millis(1);
        assert_eq!(j.liveness_at(almost), Liveness::Moving);

        let past = j.last_advance + SLOW_AFTER + std::time::Duration::from_secs(5);
        match j.liveness_at(past) {
            Liveness::Slow(idle) => assert!(idle >= SLOW_AFTER, "{idle:?}"),
            other => panic!("expected Slow, got {other:?}"),
        }

        // And the threshold is well under the engine's abort window, so the
        // user is told while the download can still recover rather than at
        // the instant it dies (#574 aborts at 120s).
        assert!(SLOW_AFTER < std::time::Duration::from_secs(120));
        j.downloaded = 1;
    }

    #[test]
    fn a_stall_is_distinguished_from_a_transport_error() {
        use crate::ui::whisper_models_state::is_stall_failure;
        // The exact wording download_stall.rs emits.
        assert!(is_stall_failure(
            "download stalled: no data received for 120s (VOICEPI_MODEL_DOWNLOAD_IDLE_TIMEOUT_SECS)"
        ));
        // The wording a real transport error keeps -- deliberately different,
        // because the two point at different remedies.
        assert!(!is_stall_failure("download read failed: connection reset"));
        assert!(!is_stall_failure("sha256 mismatch"));
    }

    #[test]
    fn status_label_missing_when_neither_cached_nor_job() {
        let (text, _) = whisper_model_status_label(false, None, egui::Color32::WHITE);
        assert_eq!(text, "Missing");
    }

    #[test]
    fn status_label_downloaded_when_cached_and_no_job() {
        let (text, _) = whisper_model_status_label(true, None, egui::Color32::WHITE);
        assert_eq!(text, "Downloaded");
    }

    #[test]
    fn status_label_in_progress_overrides_cached() {
        // Even if a valid older copy is on disk, an active redownload must
        // show "Downloading" so the user sees the work happening.
        let j = job(DownloadStatus::InProgress);
        let (text, _) = whisper_model_status_label(true, Some(&j), egui::Color32::WHITE);
        assert_eq!(text, "Downloading");
    }

    #[test]
    fn local_only_mode_disables_model_downloads_with_a_remedy() {
        assert_eq!(
            whisper_download_disabled_reason(true, false, true),
            Some("Downloads are disabled in local-only mode. Install the model manually or disable local-only mode.")
        );
        assert_eq!(whisper_download_disabled_reason(false, false, true), None);
    }

    #[test]
    fn status_label_failed_overrides_cached() {
        // Critical: a failed redownload (e.g. SHA-256 mismatch) must NOT
        // be papered over by the "Downloaded" cached badge.
        let j = job(DownloadStatus::Failed("hash mismatch".to_owned()));
        let (text, _) = whisper_model_status_label(true, Some(&j), egui::Color32::WHITE);
        assert_eq!(text, "Failed");
    }

    #[test]
    fn status_label_done_uses_success_color() {
        let j = job(DownloadStatus::Done(PathBuf::from("/x")));
        let (text, color) = whisper_model_status_label(false, Some(&j), egui::Color32::WHITE);
        assert_eq!(text, "Downloaded");
        // Sanity: success colour is greenish (G > R) so it isn't accidentally
        // wired to the failure red.
        let (r, g, _, _) = (color.r(), color.g(), color.b(), color.a());
        assert!(
            g > r,
            "expected greenish ok colour, got rgb({r},{g},{})",
            color.b()
        );
    }

    #[test]
    fn status_label_failed_uses_warning_color() {
        let j = job(DownloadStatus::Failed("net".to_owned()));
        let (text, color) = whisper_model_status_label(false, Some(&j), egui::Color32::WHITE);
        assert_eq!(text, "Failed");
        let (r, g, _, _) = (color.r(), color.g(), color.b(), color.a());
        assert!(
            r > g,
            "expected reddish failure colour, got rgb({r},{g},{})",
            color.b()
        );
    }

    #[test]
    fn status_label_marks_a_cancelled_download() {
        let j = job(DownloadStatus::Cancelled);
        let (text, _) = whisper_model_status_label(false, Some(&j), egui::Color32::WHITE);
        assert_eq!(text, "Cancelled");
    }
}
