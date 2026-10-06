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

mod verification;
use verification::file_fingerprint;
#[cfg(test)]
use verification::model_metadata;

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::whisper::download_stall::DownloadCancellation;
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
    /// The user cancelled the transfer before the model was installed.
    Cancelled,
    /// Download failed; the partial file (if any) has been deleted.
    Failed(String),
}

/// Cached-model availability without blocking the UI on a checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelAvailability {
    Available,
    Checking,
    Missing,
}

/// How long without a single byte before the UI calls a download slow.
///
/// Deliberately far below the engine's 120 s abort window. The point is
/// to tell the user something is wrong while there is still time for it to
/// recover -- a multi-GB download that goes quiet for 20 seconds and then
/// resumes is normal, but looking identical to a healthy one for two full
/// minutes is not. Duplicating the abort threshold here would mean the UI only
/// ever said anything at the instant the download died.
pub const SLOW_AFTER: Duration = Duration::from_secs(15);

/// Whether bytes are still arriving, distinct from whether the download has
/// failed.
///
/// The engine has a stalled/alive distinction in the time domain; this
/// is the same distinction made visible. Without it, `InProgress` covers both
/// "downloading at 40 MB/s" and "silent for 90 seconds and about to be
/// killed", and the user cannot tell which they are looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// A byte arrived within [`SLOW_AFTER`].
    Moving,
    /// Nothing has arrived for this long. Not yet a failure -- the engine
    /// will keep waiting until its own window elapses.
    Slow(Duration),
}

/// True when a failure came from the idle-window detector rather than from
/// a transport error.
///
/// The two point at different remedies -- a stall is "the server went quiet,
/// try again or raise the window", a transport error is "the connection
/// broke" -- so they should not render identically. Matches on the wording
/// `download_stall` deliberately made distinct from `download read failed`.
pub fn is_stall_failure(message: &str) -> bool {
    message.contains("download stalled")
}

/// Live state for one model download. The `downloaded` / `total` fields are
/// owned by the worker thread (via `on_progress`); the UI reads them each
/// frame without acquiring exclusive ownership beyond the shared mutex.
#[derive(Debug, Clone)]
pub struct DownloadJob {
    pub status: DownloadStatus,
    pub downloaded: u64,
    pub total: Option<u64>,
    /// When `downloaded` last actually ADVANCED.
    ///
    /// Not "when on_progress last fired": a callback that reports the same
    /// byte count is not progress, and treating it as such would make a
    /// stalled transfer look alive for as long as the reader kept polling.
    pub last_advance: Instant,
}

impl DownloadJob {
    /// Compute a 0.0..=1.0 progress fraction, or `None` when the total
    /// isn't known yet (server didn't send `Content-Length`). The UI shows
    /// an indeterminate spinner in that case.
    /// Liveness at an explicit instant.
    ///
    /// Takes `now` rather than reading the clock so the thresholds are
    /// testable without sleeping -- the same seam `resolve_api_key_with` uses
    /// for env lookups.
    pub fn liveness_at(&self, now: Instant) -> Liveness {
        let idle = now.saturating_duration_since(self.last_advance);
        if idle >= SLOW_AFTER {
            Liveness::Slow(idle)
        } else {
            Liveness::Moving
        }
    }

    /// Liveness now. See [`DownloadJob::liveness_at`].
    pub fn liveness(&self) -> Liveness {
        self.liveness_at(Instant::now())
    }

    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            return None;
        }
        let clamped = (self.downloaded as f64 / total as f64).clamp(0.0, 1.0);
        Some(clamped as f32)
    }
}

/// Mtime + size fingerprint from the last successful SHA-256 verification.
#[derive(Debug, Clone)]
struct VerifyCacheEntry {
    mtime: std::time::SystemTime,
    len: u64,
    verdict: VerificationVerdict,
    fingerprint: Option<u64>,
    checked_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerificationVerdict {
    Verified,
    /// A checksum mismatch that gets one delayed recheck for same-metadata repairs.
    Mismatch,
    /// A second mismatch; keep it cached until the file metadata changes.
    MismatchFinal,
    /// A delayed recheck has been scheduled for the first mismatch.
    MismatchRetryPending,
    RetryableError,
}

const FAILED_VERIFICATION_RETRY_AFTER: Duration = Duration::from_secs(5);

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
    cancellations: HashMap<&'static str, DownloadCancellation>,
    /// Cached verification results: mtime+size fingerprint → verdict.
    /// Avoids rehashing multi-hundred-MB models on every egui repaint.
    verify_cache: HashMap<&'static str, VerifyCacheEntry>,
    /// Names whose SHA-256 is being computed on a background thread.
    verify_running: std::collections::HashSet<&'static str>,
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

    /// Whether a new transfer for `name` can be started without leaving an
    /// earlier blocking request or body reader alive in the background.
    pub fn can_start(&self, name: &str) -> bool {
        let Ok(state) = self.inner.lock() else {
            return false;
        };
        !matches!(
            state.jobs.get(name),
            Some(DownloadJob {
                status: DownloadStatus::InProgress,
                ..
            })
        ) && !state
            .cancellations
            .values()
            .any(DownloadCancellation::has_active_workers)
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
        let in_progress = matches!(
            state.jobs.get(name),
            Some(DownloadJob {
                status: DownloadStatus::InProgress,
                ..
            })
        );
        let worker_is_alive = state
            .cancellations
            .values()
            .any(DownloadCancellation::has_active_workers);
        if in_progress || worker_is_alive {
            return false;
        }
        state.jobs.insert(
            name,
            DownloadJob {
                status: DownloadStatus::InProgress,
                downloaded: 0,
                total: None,
                last_advance: Instant::now(),
            },
        );
        state
            .cancellations
            .insert(name, DownloadCancellation::for_model(name));
        true
    }

    /// Request cancellation of the active download for `name`.
    pub fn cancel(&self, name: &str) -> bool {
        let Ok(state) = self.inner.lock() else {
            return false;
        };
        let Some(job) = state.jobs.get(name) else {
            return false;
        };
        if !matches!(job.status, DownloadStatus::InProgress) {
            return false;
        }
        let Some(cancellation) = state.cancellations.get(name) else {
            return false;
        };
        cancellation.cancel();
        if crate::diag::debug_enabled() {
            crate::diag::log!(
                "[models] debug: cancellation requested for {name} (active_workers={})",
                cancellation.active_workers()
            );
        }
        true
    }

    /// Request cancellation for every active transfer.
    pub fn cancel_all(&self) -> usize {
        let Ok(state) = self.inner.lock() else {
            return 0;
        };
        let mut cancelled = 0;
        for (name, job) in &state.jobs {
            if !matches!(job.status, DownloadStatus::InProgress) {
                continue;
            }
            if let Some(cancellation) = state.cancellations.get(name) {
                cancellation.cancel();
                cancelled += 1;
            }
        }
        cancelled
    }

    fn cancellation(&self, name: &str) -> Option<DownloadCancellation> {
        self.inner.lock().ok()?.cancellations.get(name).cloned()
    }

    /// Mark `name`'s job as successfully completed.
    pub fn finish_ok(&self, name: &'static str, path: PathBuf) {
        if let Ok(mut state) = self.inner.lock() {
            state.cancellations.remove(name);
            // Populate the verify cache immediately so the next render frame
            // sees "Downloaded" without scheduling a background re-hash.
            if let Ok(meta) = path.metadata() {
                if let Ok(mtime) = meta.modified() {
                    state.verify_cache.insert(
                        name,
                        VerifyCacheEntry {
                            mtime,
                            len: meta.len(),
                            verdict: VerificationVerdict::Verified,
                            fingerprint: file_fingerprint(&path),
                            checked_at: Instant::now(),
                        },
                    );
                }
            }
            state.jobs.insert(
                name,
                DownloadJob {
                    status: DownloadStatus::Done(path),
                    downloaded: 0,
                    total: None,
                    last_advance: Instant::now(),
                },
            );
        }
    }

    /// Mark `name`'s job as failed with the given message.
    pub fn finish_err(&self, name: &'static str, msg: String) {
        if let Ok(mut state) = self.inner.lock() {
            state.cancellations.remove(name);
            state.jobs.insert(
                name,
                DownloadJob {
                    status: DownloadStatus::Failed(msg),
                    downloaded: 0,
                    total: None,
                    last_advance: Instant::now(),
                },
            );
        }
    }

    /// Mark `name`'s job as cancelled after its partial file was removed.
    pub fn finish_cancelled(&self, name: &'static str) {
        if let Ok(mut state) = self.inner.lock() {
            let active_workers = state
                .cancellations
                .get(name)
                .map_or(0, DownloadCancellation::active_workers);
            if active_workers == 0 {
                state.cancellations.remove(name);
            }
            if crate::diag::debug_enabled() {
                crate::diag::log!(
                    "[models] debug: cancellation completed for {name} (active_workers={active_workers})"
                );
            }
            state.jobs.insert(
                name,
                DownloadJob {
                    status: DownloadStatus::Cancelled,
                    downloaded: 0,
                    total: None,
                    last_advance: Instant::now(),
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
                // Only mutate the moving fields -- the status stays
                // `InProgress` until `finish_ok` / `finish_err` flips it.
                //
                // The timestamp moves only on a REAL advance. A callback that
                // fires with an unchanged byte count is not progress, and
                // resetting the clock for it would make a stalled transfer
                // look alive for as long as anything kept polling.
                if downloaded > job.downloaded {
                    job.last_advance = Instant::now();
                }
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
///
/// Returns `false` (and does not spawn) when `VOICEPI_LOCAL_ONLY` is set so
/// the UI never initiates outbound network requests in privacy mode.
pub fn spawn_download(state: &WhisperModelDownloads, name: &'static str) -> bool {
    if crate::whisper::model_manager::is_local_only() {
        return false;
    }
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
    let cancellation = state
        .cancellation(name)
        .expect("active download has a cancellation token");
    let state = state.clone();
    std::thread::spawn(move || {
        let progress = state.progress_callback(name);
        match model_manager::download_model_cancellable(entry, &*progress, cancellation.clone()) {
            Ok(path) => state.finish_ok(name, path),
            Err(_err) if cancellation.is_cancelled() => state.finish_cancelled(name),
            Err(err) => state.finish_err(name, err.to_string()),
        }
    });
    true
}

#[cfg(test)]
#[path = "whisper_models_state/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "whisper_models_state_tests.rs"]
mod cancellation_tests;
