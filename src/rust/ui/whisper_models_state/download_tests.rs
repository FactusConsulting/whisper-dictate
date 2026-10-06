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
    // the idle-window detector exists to catch.
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
fn cancel_all_requests_every_active_download() {
    let state = WhisperModelDownloads::new();
    assert!(state.start("tiny.en"));
    assert!(state.start("base.en"));

    assert_eq!(state.cancel_all(), 2);
    assert!(state
        .cancellation("tiny.en")
        .expect("tiny.en cancellation")
        .is_cancelled());
    assert!(state
        .cancellation("base.en")
        .expect("base.en cancellation")
        .is_cancelled());
}
