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
use crate::whisper::model_manager::{self, ModelEntry};
use crate::whisper::models_cli::human_bytes;

impl WhisperDictateApp {
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
        for entry in model_manager::CATALOG {
            self.render_whisper_model_row(ui, entry, any_running);
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
    ) {
        let job = self.whisper_model_downloads.job(entry.name);
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
            // Disable the button while ANY download is running so the user
            // can't kick off three multi-hundred-MB downloads at once. The
            // already-cached case still allows a redownload (useful if the
            // file got corrupted out-of-band).
            let button_label = if in_progress {
                "Downloading…"
            } else if already_cached {
                "Redownload"
            } else if matches!(
                job.as_ref().map(|j| &j.status),
                Some(crate::ui::whisper_models_state::DownloadStatus::Failed(_)),
            ) {
                "Retry"
            } else {
                "Download"
            };
            let enabled = !any_running;
            if ui
                .add_enabled(enabled, egui::Button::new(button_label))
                .on_hover_text(format!(
                    "Download {} from {} to the user cache and verify its SHA-256.",
                    entry.name, entry.url
                ))
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
                crate::ui::whisper_models_state::DownloadStatus::Failed(_) => {}
            }
        }
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
        }
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
}
