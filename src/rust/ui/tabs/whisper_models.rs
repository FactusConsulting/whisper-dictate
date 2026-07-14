//! Speech-tab helpers for the Whisper model selector's auto-download flow +
//! the discovered custom-models list.
//!
//! Post-simplification: the "Whisper model downloads" section with one
//! Download button per catalog entry has been removed. The Speech-tab
//! dropdown itself is now the single source of truth for model choice — a
//! selection change to a not-yet-cached catalog model auto-triggers the
//! download, and the compact status line below the dropdown reflects
//! progress / errors / a manual "Download now" button for the case where
//! the currently-selected model just isn't on disk yet.
//!
//! Kept here (small enough to still make the file's purpose obvious):
//! - `should_auto_download` — pure predicate encoding the trigger rules.
//! - `auto_download_if_needed` — thin wrapper that resolves catalog + state
//!   and forwards to `whisper_models_state::spawn_download`.
//! - `dropdown_status_suffix` — the trailing "✓ cached / downloading NN% /
//!   size" fragment rendered inside every dropdown entry so users see each
//!   model's state without opening a second panel.
//! - `render_selected_whisper_model_status` — the inline compact status
//!   line rendered directly under the dropdown (progress bar while a
//!   download runs, red error + Retry on failure, a manual Download
//!   affordance when the model isn't cached yet).
//! - `render_discovered_custom_models_section` — the "Custom user models"
//!   list unchanged from before.
//!
//! The old per-catalog-entry rows + status-label helper have been deleted
//! along with the download section that hosted them.

use super::super::*;
use crate::ui::whisper_models_state::{
    spawn_download, DownloadJob, DownloadStatus, WhisperModelDownloads,
};
use crate::whisper::model_manager::{self, ModelEntry};
use crate::whisper::models_cli::human_bytes;

/// Pure predicate: given the current cache / local-only / in-progress
/// state, should the auto-download trigger fire for the selected model?
///
/// The trigger fires only when ALL are true:
/// - `in_catalog` — the name resolves to a catalog `ModelEntry` (so we
///   know the URL + expected SHA-256).
/// - `!local_only` — privacy mode disables silent outbound requests.
/// - `!already_cached` — no point re-downloading a verified file.
/// - `!in_progress` — a download for this model is already running.
///
/// Split from `auto_download_if_needed` so the decision matrix is
/// unit-testable without spawning threads.
pub(in crate::ui) fn should_auto_download(
    in_catalog: bool,
    local_only: bool,
    already_cached: bool,
    in_progress: bool,
) -> bool {
    in_catalog && !local_only && !already_cached && !in_progress
}

/// Auto-download entry point invoked from the render code when the model
/// dropdown selection changed. Resolves the catalog entry + queries the
/// live cache/local-only/in-progress state, applies [`should_auto_download`],
/// and forwards to [`spawn_download`] on the yes branch. Returns `true`
/// when a download thread was actually spawned.
pub(in crate::ui) fn auto_download_if_needed(
    downloads: &WhisperModelDownloads,
    model_name: &str,
) -> bool {
    let Some(entry) = model_manager::find(model_name) else {
        return false;
    };
    let already_cached = downloads.is_verified_fast(entry);
    let in_progress = matches!(
        downloads.job(entry.name).map(|j| j.status),
        Some(DownloadStatus::InProgress),
    );
    let local_only = model_manager::is_local_only();
    if !should_auto_download(true, local_only, already_cached, in_progress) {
        return false;
    }
    spawn_download(downloads, entry.name)
}

/// The trailing status marker shown in each dropdown entry:
/// - "✓ cached" when the model is on disk + verified,
/// - "· downloading NN%" (or "· downloading (X so far)" when the server
///   didn't send `Content-Length`) while a download runs,
/// - "· download failed" for a Failed job that hasn't been retried,
/// - "· XX MB" for a missing catalog entry so the user sees the download
///   size before they commit,
/// - "" for a value that isn't in the catalog (nothing meaningful to say).
///
/// Split as a pure function so the branch matrix is unit-testable without
/// an egui context.
pub(in crate::ui) fn dropdown_status_suffix(
    entry: Option<&'static ModelEntry>,
    job: Option<&DownloadJob>,
    cached: bool,
) -> String {
    if let Some(job) = job {
        match &job.status {
            DownloadStatus::InProgress => {
                return match job.fraction() {
                    Some(f) => format!("· downloading {}%", (f * 100.0).round() as u32),
                    None => format!("· downloading ({} so far)", human_bytes(job.downloaded)),
                };
            }
            DownloadStatus::Failed(_) => return "· download failed".to_owned(),
            DownloadStatus::Done(_) => { /* fall through to cached ✓ */ }
        }
    }
    if cached {
        return "✓ cached".to_owned();
    }
    if let Some(e) = entry {
        return format!("· {}", human_bytes(e.size_bytes));
    }
    String::new()
}

impl WhisperDictateApp {
    /// Render one row per auto-discovered custom GGML file the user dropped
    /// into the models cache directory (#332). Nothing is rendered when the
    /// directory has no custom files.
    pub(in crate::ui) fn render_discovered_custom_models_section(&mut self, ui: &mut egui::Ui) {
        let Ok(dir) = model_manager::models_cache_dir() else {
            return;
        };
        let discovered = crate::whisper::local_discovery::discover_models(&dir);
        if discovered.is_empty() {
            return;
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Custom user models").strong());
        ui.label(
            egui::RichText::new(
                "GGML files you dropped into the cache directory. \
                 Selected automatically when no curated model is downloaded \
                 and VOICEPI_WHISPER_MODEL_PATH is unset.",
            )
            .small()
            .weak(),
        );
        ui.add_space(2.0);
        for model in &discovered {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&model.filename).strong().monospace());
                ui.label(format!(
                    "  {}  ({})  [user]",
                    human_bytes(model.size_bytes),
                    model.variant.label(),
                ));
            });
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("Cache: {}", dir.display()))
                .small()
                .weak(),
        );
    }

    /// Render the compact download-status line shown directly under the
    /// Whisper-model dropdown for the currently-selected model.
    ///
    /// Behaviour:
    /// - `InProgress` → progress bar (or spinner when total is unknown) plus
    ///   bytes / percentage.
    /// - `Failed` → red error text + inline "Retry" button so the user doesn't
    ///   silently see their selection stuck.
    /// - Not cached, no job, not local-only → "Not downloaded" + explicit
    ///   "Download now" button (covers the case where the user opens Settings
    ///   with a pre-existing selection that isn't on disk yet — auto-download
    ///   only fires on a selection CHANGE, not on every settings visit).
    /// - Local-only + not cached → an amber note explaining why nothing
    ///   downloads automatically.
    /// - `Done` / cached / non-catalog model → nothing (keeps the row tight in
    ///   the common case — the ✓ in the dropdown label already communicates
    ///   readiness).
    pub(in crate::ui) fn render_selected_whisper_model_status(&mut self, ui: &mut egui::Ui) {
        let Some(entry) = model_manager::find(&self.settings.model) else {
            return;
        };
        let job = self.whisper_model_downloads.job(entry.name);
        let cached = self.whisper_model_downloads.is_verified_fast(entry);
        let local_only = model_manager::is_local_only();
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if let Some(job) = &job {
                match &job.status {
                    DownloadStatus::InProgress => {
                        match job.fraction() {
                            Some(f) => {
                                ui.add(
                                    egui::ProgressBar::new(f)
                                        .desired_width(220.0)
                                        .show_percentage(),
                                );
                                ui.label(format!(
                                    "{} — {} / {}",
                                    entry.name,
                                    human_bytes(job.downloaded),
                                    human_bytes(job.total.unwrap_or(job.downloaded)),
                                ));
                            }
                            None => {
                                ui.add(egui::Spinner::new());
                                ui.label(format!(
                                    "Downloading {} ({} so far)",
                                    entry.name,
                                    human_bytes(job.downloaded),
                                ));
                            }
                        }
                        return;
                    }
                    DownloadStatus::Failed(msg) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 80, 80),
                            format!("Download of {} failed: {msg}", entry.name),
                        );
                        if ui.button("Retry").clicked() {
                            let _ = spawn_download(&self.whisper_model_downloads, entry.name);
                            self.settings_status =
                                format!("Downloading Whisper model {}…", entry.name);
                        }
                        return;
                    }
                    DownloadStatus::Done(_) => { /* fall through */ }
                }
            }
            if cached {
                return;
            }
            if local_only {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 180, 80),
                    format!(
                        "{} not downloaded — local-only mode blocks auto-download.",
                        entry.name
                    ),
                );
                return;
            }
            ui.colored_label(
                egui::Color32::from_rgb(220, 180, 80),
                format!(
                    "{} not downloaded ({}).",
                    entry.name,
                    human_bytes(entry.size_bytes),
                ),
            );
            if ui.button("Download now").clicked() {
                let started = spawn_download(&self.whisper_model_downloads, entry.name);
                if started {
                    self.settings_status = format!("Downloading Whisper model {}…", entry.name);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::whisper_models_state::{DownloadJob, DownloadStatus};
    use crate::whisper::model_manager;
    use std::path::PathBuf;

    fn job(status: DownloadStatus, downloaded: u64, total: Option<u64>) -> DownloadJob {
        DownloadJob {
            status,
            downloaded,
            total,
        }
    }

    // ── should_auto_download predicate ──────────────────────────────────────

    #[test]
    fn auto_download_fires_when_missing_catalog_model_selected() {
        // Green path: a valid catalog entry that's not cached, no in-flight
        // download, not in local-only mode → the dropdown selection change
        // must trigger `spawn_download`.
        assert!(should_auto_download(
            /* in_catalog */ true, /* local_only */ false,
            /* already_cached */ false, /* in_progress */ false,
        ));
    }

    #[test]
    fn auto_download_suppressed_when_target_already_cached() {
        // Requirement: no re-download of a verified file when the user picks
        // it again.
        assert!(!should_auto_download(true, false, true, false));
    }

    #[test]
    fn auto_download_suppressed_in_local_only_mode() {
        // Requirement: local-only mode must not silently exfiltrate a
        // download request even when the user picks a missing model.
        assert!(!should_auto_download(true, true, false, false));
    }

    #[test]
    fn auto_download_suppressed_when_selection_is_not_in_catalog() {
        // A model name we don't know how to fetch (e.g. a custom user file
        // referenced only by env var) must not trigger the auto path.
        assert!(!should_auto_download(false, false, false, false));
    }

    #[test]
    fn auto_download_suppressed_when_already_in_progress() {
        // Guards against re-spawning a second worker on top of a running
        // one when the render loop re-runs the trigger for the same model.
        assert!(!should_auto_download(true, false, false, true));
    }

    // ── auto_download_if_needed (thin integration wrapper) ──────────────────

    use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};

    /// Platform-specific env var that controls the OS user-cache directory,
    /// mirroring `model_manager::user_cache_dir`'s resolution order.
    const CACHE_ENV_VAR: &str = if cfg!(windows) {
        "LOCALAPPDATA"
    } else if cfg!(target_os = "macos") {
        "HOME"
    } else {
        "XDG_CACHE_HOME"
    };

    #[test]
    fn auto_download_if_needed_returns_false_for_unknown_model() {
        // A name that isn't in the catalog can't be auto-fetched (no URL /
        // SHA-256 to hit). Must return false without touching download state.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
        let downloads = WhisperModelDownloads::new();
        assert!(!auto_download_if_needed(
            &downloads,
            "totally-unknown-model"
        ));
    }

    #[test]
    fn auto_download_if_needed_returns_false_when_local_only() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvVarGuard::set("VOICEPI_LOCAL_ONLY", "1");
        let downloads = WhisperModelDownloads::new();
        assert!(
            !auto_download_if_needed(&downloads, "tiny.en"),
            "local-only mode must suppress the auto-trigger"
        );
        assert!(
            downloads.job("tiny.en").is_none(),
            "no job slot must be created when local-only blocks the download"
        );
    }

    #[test]
    fn auto_download_if_needed_returns_false_when_already_in_progress() {
        // Simulate the "user changes selection twice in rapid succession"
        // race by pre-reserving the slot, then confirming the second trigger
        // is a no-op.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
        let downloads = WhisperModelDownloads::new();
        assert!(downloads.start("tiny.en"), "prep: reserve slot");
        assert!(
            !auto_download_if_needed(&downloads, "tiny.en"),
            "auto-trigger must refuse when a download for the same model is in flight"
        );
    }

    #[test]
    fn auto_download_if_needed_returns_false_when_target_cached() {
        // Point the user-cache env var at a tempdir and drop a real file at
        // the resolved model path so `is_verified_fast` schedules a verify
        // pass; we then finish_ok to populate the verify cache with the
        // matching mtime+len and confirm the auto-trigger does NOT re-spawn.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _local_only_guard = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
        let tmp = tempfile::tempdir().unwrap();
        let _cache_guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());

        let entry = model_manager::find("tiny.en").unwrap();
        let model_path = model_manager::model_path(entry).unwrap();
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        std::fs::write(&model_path, b"stand-in").unwrap();

        let downloads = WhisperModelDownloads::new();
        // finish_ok populates the verify_cache so `is_verified_fast` returns
        // true synchronously on the next call — mimics the "already cached"
        // path without needing a real SHA-256 match.
        downloads.start("tiny.en");
        downloads.finish_ok("tiny.en", model_path.clone());

        // Clear the Done job so the InProgress guard doesn't shadow the
        // cached guard — we want to isolate the "already_cached" branch.
        // (Done still counts as job present but status_is_in_progress=false.)
        let triggered = auto_download_if_needed(&downloads, "tiny.en");
        assert!(!triggered, "cached model must not re-trigger auto-download");
    }

    // ── dropdown_status_suffix formatter ────────────────────────────────────

    #[test]
    fn suffix_for_cached_catalog_entry_is_the_check_mark() {
        let entry = model_manager::find("tiny.en");
        assert_eq!(
            dropdown_status_suffix(entry, None, /* cached */ true),
            "✓ cached"
        );
    }

    #[test]
    fn suffix_for_missing_catalog_entry_shows_size() {
        let entry = model_manager::find("large-v3").unwrap();
        let suffix = dropdown_status_suffix(Some(entry), None, false);
        assert!(
            suffix.starts_with("· "),
            "size suffix must be prefixed with a bullet, got {suffix:?}"
        );
        // The whisper large-v3 GGML is ~3.1 GB — the human_bytes formatter
        // should render "GB" (or the closest unit) somewhere in the suffix.
        assert!(
            suffix.contains("GB") || suffix.contains("MB"),
            "expected human-readable size in {suffix:?}"
        );
    }

    #[test]
    fn suffix_for_in_progress_job_with_total_shows_percent() {
        let entry = model_manager::find("tiny.en");
        let j = job(DownloadStatus::InProgress, 40, Some(100));
        assert_eq!(
            dropdown_status_suffix(entry, Some(&j), false),
            "· downloading 40%"
        );
    }

    #[test]
    fn suffix_for_in_progress_job_without_total_shows_bytes_only() {
        // Server without a Content-Length header — the dropdown falls back
        // to a rolling byte counter instead of a fake percentage.
        let entry = model_manager::find("tiny.en");
        let j = job(DownloadStatus::InProgress, 1024, None);
        let suffix = dropdown_status_suffix(entry, Some(&j), false);
        assert!(
            suffix.starts_with("· downloading ("),
            "byte-fallback suffix must not lie about a percent, got {suffix:?}"
        );
    }

    #[test]
    fn suffix_for_failed_job_flags_failure() {
        // A silent revert to the size marker would hide the failure from
        // the user; the dropdown must show "download failed" so the Retry
        // button under the row is discoverable.
        let entry = model_manager::find("tiny.en");
        let j = job(DownloadStatus::Failed("hash mismatch".to_owned()), 0, None);
        assert_eq!(
            dropdown_status_suffix(entry, Some(&j), false),
            "· download failed"
        );
    }

    #[test]
    fn suffix_for_unknown_entry_and_no_job_is_empty() {
        // A custom/user model referenced only by env var isn't in the
        // catalog and has no download job. The dropdown must render just
        // the base name+hint, with no trailing marker.
        assert_eq!(dropdown_status_suffix(None, None, false), "");
    }

    #[test]
    fn suffix_prefers_cached_when_done_job_and_verified() {
        // After finish_ok the job is Done AND the verify cache is populated.
        // The suffix must fall through to the "✓ cached" branch (Done alone
        // isn't a persistent state — the ✓ is what the user should see).
        let entry = model_manager::find("tiny.en");
        let j = job(DownloadStatus::Done(PathBuf::from("/x")), 0, None);
        assert_eq!(
            dropdown_status_suffix(entry, Some(&j), /* cached */ true),
            "✓ cached"
        );
    }
}
