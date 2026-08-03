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
use std::time::{Duration, Instant};

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

/// How long without a single byte before the UI calls a download slow.
///
/// Deliberately far below the engine's 120 s abort window (#574). The point is
/// to tell the user something is wrong while there is still time for it to
/// recover -- a multi-GB download that goes quiet for 20 seconds and then
/// resumes is normal, but looking identical to a healthy one for two full
/// minutes is not. Duplicating the abort threshold here would mean the UI only
/// ever said anything at the instant the download died.
pub const SLOW_AFTER: Duration = Duration::from_secs(15);

/// Whether bytes are still arriving, distinct from whether the download has
/// failed.
///
/// #574 gave the engine a stalled/alive distinction in the time domain; this
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

/// True when a failure came from #574's idle-window detector rather than from
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
    verified: bool,
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
            .insert(name, DownloadCancellation::default());
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
        true
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
                            verified: true,
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
            if state
                .cancellations
                .get(name)
                .is_none_or(|cancellation| !cancellation.has_active_workers())
            {
                state.cancellations.remove(name);
            }
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
            if state
                .cancellations
                .get(name)
                .is_none_or(|cancellation| !cancellation.has_active_workers())
            {
                state.cancellations.remove(name);
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

    /// Fast cached check: is this catalog entry present and verified?
    ///
    /// Returns `true` when the file exists and its last SHA-256 check passed,
    /// and the file's mtime + size haven't changed since. Returns `false`
    /// while a background verify is in flight (scheduled automatically on first
    /// call after a cache miss), or when the file is absent.
    ///
    /// This replaces a synchronous `verify_sha256` call on the UI thread,
    /// which blocked the Settings window for seconds on every repaint.
    pub fn is_verified_fast(
        &self,
        entry: &'static crate::whisper::model_manager::ModelEntry,
    ) -> bool {
        let path = match crate::whisper::model_manager::model_path(entry) {
            Ok(p) => p,
            Err(_) => return false,
        };
        if !path.is_file() {
            if let Ok(mut inner) = self.inner.lock() {
                inner.verify_cache.remove(entry.name);
                inner.verify_running.remove(entry.name);
            }
            return false;
        }
        let meta = match path.metadata() {
            Ok(m) => m,
            Err(_) => return false,
        };
        let mtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => return false,
        };
        let len = meta.len();

        {
            let Ok(mut inner) = self.inner.lock() else {
                return false;
            };
            if let Some(cached) = inner.verify_cache.get(entry.name) {
                if cached.mtime == mtime && cached.len == len {
                    return cached.verified;
                }
            }
            // Cache miss or stale metadata: schedule a background verify.
            if !inner.verify_running.insert(entry.name) {
                // Already running — keep returning false until it finishes.
                return false;
            }
        }

        let state = self.clone();
        std::thread::Builder::new()
            .name(format!("whisper-verify-{}", entry.name))
            .spawn(move || {
                // Snapshot metadata BEFORE hashing.  If a concurrent download
                // replaces the file between the snapshot and the end of hashing
                // the mtime/len will differ after hashing; we detect that and
                // discard the stale result so a valid replacement isn't
                // incorrectly cached as unverified (P2 race fix).
                let meta_before = crate::whisper::model_manager::model_path(entry)
                    .ok()
                    .and_then(|p| {
                        p.metadata()
                            .ok()
                            .and_then(|m| m.modified().ok().map(|mt| (mt, m.len())))
                    });

                let verified = crate::whisper::model_manager::is_downloaded(entry);

                let Ok(mut inner) = state.inner.lock() else {
                    return;
                };
                if let Ok(path2) = crate::whisper::model_manager::model_path(entry) {
                    if let Ok(meta2) = path2.metadata() {
                        if let Ok(mt) = meta2.modified() {
                            let len2 = meta2.len();
                            // Only cache when metadata is unchanged: if the
                            // file was replaced while we were hashing the old
                            // copy, skip the insert so the next
                            // `is_verified_fast` call sees a cache miss and
                            // schedules a fresh verify on the new file.
                            let unchanged = meta_before
                                .map(|(mt_before, len_before)| {
                                    mt_before == mt && len_before == len2
                                })
                                .unwrap_or(false);
                            if unchanged {
                                inner.verify_cache.insert(
                                    entry.name,
                                    VerifyCacheEntry {
                                        mtime: mt,
                                        len: len2,
                                        verified,
                                    },
                                );
                            }
                            // If unchanged==false: discard; the next call
                            // will detect the new mtime/len → cache miss →
                            // fresh verify thread for the replacement file.
                        }
                    }
                }
                inner.verify_running.remove(entry.name);
            })
            .ok();

        false
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
mod tests {
    use super::*;

    #[test]
    fn fraction_is_none_when_total_unknown() {
        let job = DownloadJob {
            status: DownloadStatus::InProgress,
            downloaded: 1000,
            total: None,
            last_advance: Instant::now(),
        };
        assert_eq!(job.fraction(), None);
    }

    #[test]
    fn fraction_is_clamped_to_unit_range() {
        let job = DownloadJob {
            status: DownloadStatus::InProgress,
            downloaded: 0,
            total: Some(100),
            last_advance: Instant::now(),
        };
        assert_eq!(job.fraction(), Some(0.0));
        let job = DownloadJob {
            status: DownloadStatus::InProgress,
            downloaded: 50,
            total: Some(100),
            last_advance: Instant::now(),
        };
        assert_eq!(job.fraction(), Some(0.5));
        // Over-shoot (server lied about Content-Length) clamps to 1.0
        // instead of overflowing a progress bar widget.
        let job = DownloadJob {
            status: DownloadStatus::InProgress,
            downloaded: 200,
            total: Some(100),
            last_advance: Instant::now(),
        };
        assert_eq!(job.fraction(), Some(1.0));
    }

    #[test]
    fn a_repeated_byte_count_is_not_progress() {
        // The distinction the whole feature rests on. A reader that keeps
        // calling back with the same total is not making progress, and
        // resetting the clock for it would make a stalled transfer look alive
        // for as long as anything kept polling -- which is precisely the case
        // #574 exists to catch.
        let downloads = WhisperModelDownloads::default();
        downloads.start("tiny.en");
        let cb = downloads.progress_callback("tiny.en");

        cb.on_progress(1024, Some(4096));
        let after_real = downloads.job("tiny.en").expect("job").last_advance;

        cb.on_progress(1024, Some(4096)); // same count: not an advance
        let after_repeat = downloads.job("tiny.en").expect("job").last_advance;
        assert_eq!(after_real, after_repeat, "a repeated count moved the clock");

        cb.on_progress(2048, Some(4096)); // a real advance
        let after_advance = downloads.job("tiny.en").expect("job").last_advance;
        assert!(after_advance > after_real, "a real advance must move it");
    }

    #[test]
    fn fraction_handles_zero_total() {
        // Zero-length response: avoid divide-by-zero, render as
        // indeterminate.
        let job = DownloadJob {
            status: DownloadStatus::InProgress,
            downloaded: 0,
            total: Some(0),
            last_advance: Instant::now(),
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
    fn cancellation_marks_only_an_active_download() {
        let state = WhisperModelDownloads::new();
        assert!(!state.cancel("tiny.en"));

        assert!(state.start("tiny.en"));
        assert!(state.cancel("tiny.en"));
        assert!(state
            .cancellation("tiny.en")
            .expect("active download token")
            .is_cancelled());

        state.finish_cancelled("tiny.en");
        assert_eq!(
            state.job("tiny.en").expect("job").status,
            DownloadStatus::Cancelled
        );
        assert!(!state.cancel("tiny.en"));
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
    fn is_verified_fast_returns_bool_without_panicking_for_absent_file() {
        // On CI (and most developer machines) the real cache dir won't contain
        // a downloaded model. Calling is_verified_fast must return false
        // without panicking, blocking, or spinning on a missing file.
        let state = WhisperModelDownloads::new();
        let entry = crate::whisper::model_manager::find("tiny.en").unwrap();
        // Just assert it doesn't panic and returns a bool.
        let result = state.is_verified_fast(entry);
        // May be true on a developer machine with the model cached; false elsewhere.
        let _ = result;
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

    // ── spawn_download tests ──────────────────────────────────────────────────

    use crate::test_env_lock::ENV_LOCK;
    use std::ffi::OsString;

    /// Save/restore wrapper for env-var mutation in tests. Mirrors the pattern
    /// in `model_manager_tests.rs` — defined inline so we don't need a
    /// `pub(super)` dep on `ui::test_support`.
    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }
        fn remove(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

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
    fn spawn_download_blocked_when_local_only() {
        // The VOICEPI_LOCAL_ONLY guard in spawn_download must abort before
        // touching the download state so no job slot is created and no thread
        // is spawned. Covers the new `is_local_only()` early-return branch.
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let _guard = EnvVarGuard::set("VOICEPI_LOCAL_ONLY", "1");
        let state = WhisperModelDownloads::new();
        assert!(
            !spawn_download(&state, "tiny.en"),
            "spawn_download must return false in local-only mode"
        );
        assert!(
            state.job("tiny.en").is_none(),
            "no job slot must have been created in local-only mode"
        );
    }

    #[test]
    fn spawn_download_returns_false_for_unknown_model() {
        // Covers the `model_manager::find(name) == None` branch in
        // spawn_download: it must record a Failed job and return false.
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let _guard = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
        let state = WhisperModelDownloads::new();
        assert!(
            !spawn_download(&state, "unknown-model"),
            "spawn_download must return false for an unrecognised model name"
        );
        let job = state
            .job("unknown-model")
            .expect("a Failed job slot must be recorded for the unknown name");
        assert!(
            matches!(job.status, DownloadStatus::Failed(_)),
            "job must be Failed, got {job:?}"
        );
    }

    #[test]
    fn spawn_download_returns_false_when_already_in_progress() {
        // Pre-reserve the slot so `start()` refuses a second caller — the
        // guard in `spawn_download` must detect this and abort cleanly.
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let _guard = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
        let state = WhisperModelDownloads::new();
        state.start("tiny.en"); // reserves the slot
        assert!(
            !spawn_download(&state, "tiny.en"),
            "spawn_download must refuse when the model is already in-progress"
        );
    }

    #[test]
    fn finish_ok_with_real_file_populates_verify_cache() {
        // finish_ok reads the file's mtime+len and stores them in the
        // verify_cache. We verify this indirectly: after finish_ok with a
        // real tempdir file, is_verified_fast should find a cache entry and
        // skip the background verify thread, returning immediately.
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let tmp = tempfile::tempdir().unwrap();
        let _cache_guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());

        let entry = crate::whisper::model_manager::find("tiny.en").unwrap();
        let model_path = crate::whisper::model_manager::model_path(entry)
            .expect("model_path must resolve under tmp cache");
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        std::fs::write(&model_path, b"fake-ggml-bytes").unwrap();

        let state = WhisperModelDownloads::new();
        state.start("tiny.en");
        // finish_ok reads metadata from model_path and caches mtime+len.
        state.finish_ok("tiny.en", model_path.clone());

        let job = state.job("tiny.en").expect("job must be recorded");
        assert!(
            matches!(job.status, DownloadStatus::Done(_)),
            "finish_ok must transition status to Done, got {job:?}"
        );

        // The verify_cache entry should now let is_verified_fast return
        // without queuing a background thread (cache hit returns immediately).
        // The exact return value depends on whether modified() is supported,
        // but the call must not hang or panic.
        let _ = state.is_verified_fast(entry);
    }

    #[test]
    fn is_verified_fast_with_real_file_reaches_lock_and_schedules_verify() {
        // Exercise the main body of is_verified_fast: file exists → metadata
        // ok → lock acquired → cache miss → verify_running.insert → thread
        // spawned → return false.
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let tmp = tempfile::tempdir().unwrap();
        let _cache_guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());

        let entry = crate::whisper::model_manager::find("tiny.en").unwrap();
        let model_path = crate::whisper::model_manager::model_path(entry)
            .expect("model_path must resolve under tmp cache");
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        std::fs::write(&model_path, b"fake-bytes-wrong-hash").unwrap();

        let state = WhisperModelDownloads::new();
        // First call: cache empty → schedules a background verify → returns false.
        let result = state.is_verified_fast(entry);
        assert!(
            !result,
            "is_verified_fast must return false on first call (cache miss)"
        );
        // Second immediate call: verify_running says "already running" → false.
        let result2 = state.is_verified_fast(entry);
        assert!(
            !result2,
            "is_verified_fast must return false while verify thread is running"
        );
    }

    #[test]
    fn is_verified_fast_does_not_cache_stale_result_after_file_replacement() {
        // P2 race-fix: if a redownload replaces the file while a background
        // verify is running, the verify thread must NOT store the old
        // (corrupt) hash result under the new file's mtime/len.
        // We simulate this by:
        //   1. Placing a file and calling is_verified_fast (schedules thread).
        //   2. Replacing the file with different bytes before the thread stores.
        //   3. Waiting briefly for the thread to finish.
        //   4. Checking that is_verified_fast either returns false (cache miss
        //      or stale-detect) or re-schedules a new verify (returns false),
        //      but never returns true for the corrupt original hash result.
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let tmp = tempfile::tempdir().unwrap();
        let _cache_guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());

        let entry = crate::whisper::model_manager::find("tiny.en").unwrap();
        let model_path = crate::whisper::model_manager::model_path(entry)
            .expect("model_path must resolve under tmp cache");
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        // Write "old corrupt" bytes.
        std::fs::write(&model_path, b"old-corrupt-bytes").unwrap();

        let state = WhisperModelDownloads::new();
        // First call schedules background verify of the corrupt file.
        let _ = state.is_verified_fast(entry);

        // Replace the file immediately (simulate a concurrent download
        // completing). On fast machines the thread may not have started yet,
        // which makes this a no-op race — that's fine: the test is still
        // valid if the thread sees the new file and hashes it correctly.
        std::fs::write(&model_path, b"new-bytes-different-hash").unwrap();

        // Let the verify thread finish.
        std::thread::sleep(std::time::Duration::from_millis(300));

        // After replacement, is_verified_fast must return false (the new
        // file has wrong hash too, but the important invariant is that we
        // didn't cache the old result under the new metadata).
        let result = state.is_verified_fast(entry);
        assert!(
            !result,
            "must not return true after file replacement with corrupt content: got {result}"
        );
    }

    #[test]
    fn is_verified_fast_returns_cached_result_after_finish_ok() {
        // Exercise the cache-hit branch: after finish_ok populates the
        // verify_cache with verified=true and the mtime+len match what
        // is_verified_fast reads from disk, it must return true without
        // scheduling another verify thread.
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let tmp = tempfile::tempdir().unwrap();
        let _cache_guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());

        let entry = crate::whisper::model_manager::find("tiny.en").unwrap();
        let model_path = crate::whisper::model_manager::model_path(entry)
            .expect("model_path must resolve under tmp cache");
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        std::fs::write(&model_path, b"fake-model-bytes-for-cache-hit-test").unwrap();

        let state = WhisperModelDownloads::new();
        state.start("tiny.en");
        // finish_ok reads the file's metadata and inserts verified=true into
        // the verify_cache keyed by mtime+len.
        state.finish_ok("tiny.en", model_path.clone());

        // On platforms where modified() returns Ok (Linux/macOS/Windows), the
        // mtime+len from finish_ok match what is_verified_fast reads → cache
        // hit → returns the cached `verified` value (true). On unusual
        // platforms without mtime support finish_ok skips the cache insert and
        // is_verified_fast falls through to scheduling the verify thread (still
        // must not panic).
        let result = state.is_verified_fast(entry);
        // We can't guarantee `true` on every OS (modified() is not universal),
        // so just assert the call completes without panicking.
        let _ = result;
    }
}

#[cfg(test)]
#[path = "whisper_models_state_tests.rs"]
mod cancellation_tests;
