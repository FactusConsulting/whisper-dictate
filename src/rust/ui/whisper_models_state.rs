//! Shared download state for the Settings tab's "Whisper model" section.
//!
//! egui is immediate-mode, so a multi-hundred-megabyte download must run on
//! a worker thread with progress polled each frame. This module holds the
//! shared state container and the per-job records the worker thread updates
//! via the `DownloadProgress` callback. All public types are `Send + Sync`
//! so they can live behind a single `Arc<Mutex<…>>` owned by
//! `WhisperDictateApp`.
//!
//! Kept separate from the per-tab render code (`tabs/whisper_models.rs`) so
//! the state model + transitions are independently unit-testable without an
//! egui context.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::whisper::model_manager::{self, DownloadProgress};

/// One download's lifecycle, from "user clicked Download" through to a final
/// success or error verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadStatus {
    /// Bytes are being streamed to the partial file. Progress is tracked
    /// separately in [`DownloadJob`] so a render pass can show the bar
    /// without cloning the variant.
    InProgress,
    /// Download succeeded, integrity check passed, final file is at the
    /// given path.
    Done(PathBuf),
    /// Download failed; the partial file (if any) has been deleted.
    Failed(String),
}

/// Live state for one model download. The `downloaded` / `total` fields are
/// owned by the worker thread (via `on_progress`); the UI reads them each
/// frame without acquiring exclusive ownership beyond the shared mutex.
#[derive(Debug, Clone)]
pub struct DownloadJob {
    pub status: DownloadStatus,
    pub downloaded: u64,
    pub total: Option<u64>,
}

impl DownloadJob {
    /// Compute a 0.0..=1.0 progress fraction, or `None` when the total
    /// isn't known yet (server didn't send `Content-Length`). The UI shows
    /// an indeterminate spinner in that case.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            return None;
        }
        let clamped = (self.downloaded as f64 / total as f64).clamp(0.0, 1.0);
        Some(clamped as f32)
    }
}

/// In-flight downloads keyed by catalog name. `Arc<Mutex<…>>` clones share
/// the same map so the worker thread's progress updates land in the same
/// place the UI thread reads.
#[derive(Debug, Default, Clone)]
pub struct WhisperModelDownloads {
    inner: Arc<Mutex<DownloadsInner>>,
}

#[derive(Debug, Default)]
struct DownloadsInner {
    jobs: HashMap<&'static str, DownloadJob>,
}

impl WhisperModelDownloads {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the current job for `name`, if any. The clone keeps the
    /// lock window short — the UI never holds the mutex across an egui
    /// widget call.
    pub fn job(&self, name: &str) -> Option<DownloadJob> {
        self.inner.lock().ok()?.jobs.get(name).cloned()
    }

    /// True iff any catalog entry is currently being downloaded. Used to
    /// disable other Download buttons while one is in flight (avoids the
    /// user kicking off three multi-hundred-MB downloads at once).
    pub fn any_in_progress(&self) -> bool {
        let Ok(state) = self.inner.lock() else {
            return false;
        };
        state
            .jobs
            .values()
            .any(|j| matches!(j.status, DownloadStatus::InProgress))
    }

    /// Reserve a slot for `name` in the InProgress state. Returns `false`
    /// (and leaves the map untouched) if a download for `name` is already
    /// running, so the caller doesn't spawn two threads racing on the same
    /// file. Successful / failed past attempts ARE overwritten — clicking
    /// "Retry" after a failure must start a fresh job.
    pub fn start(&self, name: &'static str) -> bool {
        let Ok(mut state) = self.inner.lock() else {
            return false;
        };
        if matches!(
            state.jobs.get(name),
            Some(DownloadJob {
                status: DownloadStatus::InProgress,
                ..
            })
        ) {
            return false;
        }
        state.jobs.insert(
            name,
            DownloadJob {
                status: DownloadStatus::InProgress,
                downloaded: 0,
                total: None,
            },
        );
        true
    }

    /// Mark `name`'s job as successfully completed.
    pub fn finish_ok(&self, name: &'static str, path: PathBuf) {
        if let Ok(mut state) = self.inner.lock() {
            state.jobs.insert(
                name,
                DownloadJob {
                    status: DownloadStatus::Done(path),
                    downloaded: 0,
                    total: None,
                },
            );
        }
    }

    /// Mark `name`'s job as failed with the given message.
    pub fn finish_err(&self, name: &'static str, msg: String) {
        if let Ok(mut state) = self.inner.lock() {
            state.jobs.insert(
                name,
                DownloadJob {
                    status: DownloadStatus::Failed(msg),
                    downloaded: 0,
                    total: None,
                },
            );
        }
    }

    /// Build a [`DownloadProgress`] callback bound to `name` that updates
    /// the shared state in place. The returned trait object is `Send +
    /// Sync` so it can be moved into the download worker thread.
    pub fn progress_callback(&self, name: &'static str) -> Box<dyn DownloadProgress> {
        Box::new(ProgressBinding {
            inner: self.inner.clone(),
            name,
        })
    }
}

struct ProgressBinding {
    inner: Arc<Mutex<DownloadsInner>>,
    name: &'static str,
}

impl DownloadProgress for ProgressBinding {
    fn on_progress(&self, downloaded: u64, total: Option<u64>) {
        if let Ok(mut state) = self.inner.lock() {
            if let Some(job) = state.jobs.get_mut(self.name) {
                // Only mutate the moving fields — the status stays
                // `InProgress` until `finish_ok` / `finish_err` flips it.
                job.downloaded = downloaded;
                job.total = total;
            }
        }
    }
}

/// Spawn the background download. On success the shared state's job for
/// `name` ends up in `Done(path)`; on failure in `Failed(msg)`. The worker
/// thread is detached — egui polls the shared state each frame, so there is
/// no join handle to manage and no channel to drain.
pub fn spawn_download(state: &WhisperModelDownloads, name: &'static str) -> bool {
    if !state.start(name) {
        return false;
    }
    let entry = match model_manager::find(name) {
        Some(e) => e,
        None => {
            state.finish_err(name, format!("unknown model '{name}'"));
            return false;
        }
    };
    let state = state.clone();
    std::thread::spawn(move || {
        let progress = state.progress_callback(name);
        match model_manager::download_model(entry, &*progress) {
            Ok(path) => state.finish_ok(name, path),
            Err(err) => state.finish_err(name, err.to_string()),
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_is_none_when_total_unknown() {
        let job = DownloadJob {
            status: DownloadStatus::InProgress,
            downloaded: 1000,
            total: None,
        };
        assert_eq!(job.fraction(), None);
    }

    #[test]
    fn fraction_is_clamped_to_unit_range() {
        let job = DownloadJob {
            status: DownloadStatus::InProgress,
            downloaded: 0,
            total: Some(100),
        };
        assert_eq!(job.fraction(), Some(0.0));
        let job = DownloadJob {
            status: DownloadStatus::InProgress,
            downloaded: 50,
            total: Some(100),
        };
        assert_eq!(job.fraction(), Some(0.5));
        // Over-shoot (server lied about Content-Length) clamps to 1.0
        // instead of overflowing a progress bar widget.
        let job = DownloadJob {
            status: DownloadStatus::InProgress,
            downloaded: 200,
            total: Some(100),
        };
        assert_eq!(job.fraction(), Some(1.0));
    }

    #[test]
    fn fraction_handles_zero_total() {
        // Zero-length response: avoid divide-by-zero, render as
        // indeterminate.
        let job = DownloadJob {
            status: DownloadStatus::InProgress,
            downloaded: 0,
            total: Some(0),
        };
        assert_eq!(job.fraction(), None);
    }

    #[test]
    fn start_rejects_concurrent_download_of_same_model() {
        let state = WhisperModelDownloads::new();
        assert!(state.start("tiny.en"), "first start must succeed");
        assert!(state.any_in_progress(), "in-progress flag must flip");
        // Second start while still in-progress is refused so the UI can't
        // spawn two threads racing on the same partial file.
        assert!(
            !state.start("tiny.en"),
            "concurrent start of same model must be refused",
        );
    }

    #[test]
    fn start_allows_retry_after_failure() {
        let state = WhisperModelDownloads::new();
        assert!(state.start("tiny.en"));
        state.finish_err("tiny.en", "boom".to_owned());
        assert!(
            !state.any_in_progress(),
            "failed job no longer counts as in-progress",
        );
        // A click on "Retry" after the failure must start a fresh job.
        assert!(
            state.start("tiny.en"),
            "start after failure must succeed (retry path)",
        );
    }

    #[test]
    fn start_allows_redownload_after_success() {
        let state = WhisperModelDownloads::new();
        assert!(state.start("tiny.en"));
        state.finish_ok("tiny.en", PathBuf::from("/tmp/whatever.bin"));
        assert!(
            state.start("tiny.en"),
            "redownload after success must succeed (e.g. cache cleared)",
        );
    }

    #[test]
    fn finish_ok_transitions_to_done_with_path() {
        let state = WhisperModelDownloads::new();
        state.start("tiny.en");
        state.finish_ok("tiny.en", PathBuf::from("/cache/ggml-tiny.en.bin"));
        let job = state.job("tiny.en").expect("job recorded");
        assert_eq!(
            job.status,
            DownloadStatus::Done(PathBuf::from("/cache/ggml-tiny.en.bin"))
        );
    }

    #[test]
    fn finish_err_transitions_to_failed_with_message() {
        let state = WhisperModelDownloads::new();
        state.start("tiny.en");
        state.finish_err("tiny.en", "SHA-256 mismatch".to_owned());
        let job = state.job("tiny.en").expect("job recorded");
        match job.status {
            DownloadStatus::Failed(msg) => assert!(msg.contains("SHA-256")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn progress_callback_updates_shared_state() {
        let state = WhisperModelDownloads::new();
        state.start("tiny.en");
        let cb = state.progress_callback("tiny.en");
        cb.on_progress(1024, Some(2048));
        let job = state.job("tiny.en").expect("job recorded");
        assert_eq!(job.downloaded, 1024);
        assert_eq!(job.total, Some(2048));
        assert_eq!(job.status, DownloadStatus::InProgress);
    }

    #[test]
    fn progress_callback_for_unknown_job_is_a_noop() {
        // If the job slot was cleared between the worker's last
        // `on_progress` and now (e.g. the UI hot-reset state), the
        // callback must silently drop the update instead of panicking.
        let state = WhisperModelDownloads::new();
        let cb = state.progress_callback("tiny.en");
        cb.on_progress(42, Some(100));
        assert!(state.job("tiny.en").is_none());
    }

    #[test]
    fn any_in_progress_only_counts_running_jobs() {
        let state = WhisperModelDownloads::new();
        state.start("tiny.en");
        state.finish_ok("tiny.en", PathBuf::from("/x"));
        state.start("base.en");
        // Done + InProgress → still in progress because of base.en.
        assert!(state.any_in_progress());
        state.finish_err("base.en", "net".to_owned());
        assert!(
            !state.any_in_progress(),
            "Done + Failed should report no work in progress",
        );
    }
}
