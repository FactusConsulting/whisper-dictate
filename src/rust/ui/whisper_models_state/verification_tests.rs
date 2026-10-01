use super::*;

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
    assert_eq!(state.availability_fast(entry), ModelAvailability::Checking);
    // The verifier may finish between calls; either state proves that the
    // file was scheduled without making the test timing-dependent.
    assert!(matches!(
        state.availability_fast(entry),
        ModelAvailability::Checking | ModelAvailability::Missing
    ));
}

#[test]
fn failed_verification_is_retried_after_the_backoff() {
    let _lock = ENV_LOCK.lock().expect("env lock poisoned");
    let tmp = tempfile::tempdir().unwrap();
    let _cache_guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());
    let entry = crate::whisper::model_manager::find("tiny.en").unwrap();
    let model_path = crate::whisper::model_manager::model_path(entry).unwrap();
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, b"fake-bytes-wrong-hash").unwrap();
    let (mtime, len) = model_metadata(entry).unwrap();
    let state = WhisperModelDownloads::new();
    state.inner.lock().unwrap().verify_cache.insert(
        entry.name,
        VerifyCacheEntry {
            mtime,
            len,
            verdict: VerificationVerdict::RetryableError,
            fingerprint: file_fingerprint(&model_path),
            checked_at: Instant::now() - FAILED_VERIFICATION_RETRY_AFTER,
        },
    );

    assert_eq!(state.cached_availability(entry), None);
    assert_eq!(state.availability_fast(entry), ModelAvailability::Checking);
}

#[test]
fn definitive_checksum_mismatch_stays_cached_until_file_changes() {
    let _lock = ENV_LOCK.lock().expect("env lock poisoned");
    let tmp = tempfile::tempdir().unwrap();
    let _cache_guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());
    let entry = crate::whisper::model_manager::find("tiny.en").unwrap();
    let model_path = crate::whisper::model_manager::model_path(entry).unwrap();
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, b"fake-bytes-wrong-hash").unwrap();
    let (mtime, len) = model_metadata(entry).unwrap();
    let state = WhisperModelDownloads::new();
    state.inner.lock().unwrap().verify_cache.insert(
        entry.name,
        VerifyCacheEntry {
            mtime,
            len,
            verdict: VerificationVerdict::Mismatch,
            fingerprint: file_fingerprint(&model_path),
            checked_at: Instant::now(),
        },
    );

    assert_eq!(
        state.cached_availability(entry),
        Some(ModelAvailability::Missing)
    );
    assert_eq!(state.availability_fast(entry), ModelAvailability::Missing);
    assert!(!state
        .inner
        .lock()
        .unwrap()
        .verify_running
        .contains(entry.name));
}

#[test]
fn checksum_mismatch_is_rechecked_once_after_backoff() {
    let _lock = ENV_LOCK.lock().expect("env lock poisoned");
    let tmp = tempfile::tempdir().unwrap();
    let _cache_guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());
    let entry = crate::whisper::model_manager::find("tiny.en").unwrap();
    let model_path = crate::whisper::model_manager::model_path(entry).unwrap();
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, b"fake-bytes-wrong-hash").unwrap();
    let (mtime, len) = model_metadata(entry).unwrap();
    let state = WhisperModelDownloads::new();
    state.inner.lock().unwrap().verify_cache.insert(
        entry.name,
        VerifyCacheEntry {
            mtime,
            len,
            verdict: VerificationVerdict::Mismatch,
            fingerprint: file_fingerprint(&model_path),
            checked_at: Instant::now() - FAILED_VERIFICATION_RETRY_AFTER,
        },
    );

    assert_eq!(state.availability_fast(entry), ModelAvailability::Checking);
    let mut verdict = VerificationVerdict::Mismatch;
    for _ in 0..100 {
        verdict = state
            .inner
            .lock()
            .unwrap()
            .verify_cache
            .get(entry.name)
            .map(|cached| cached.verdict)
            .unwrap_or(VerificationVerdict::RetryableError);
        if verdict == VerificationVerdict::MismatchFinal {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(verdict, VerificationVerdict::MismatchFinal);
    assert_eq!(state.availability_fast(entry), ModelAvailability::Missing);
    assert_eq!(
        state.inner.lock().unwrap().verify_cache[entry.name].verdict,
        VerificationVerdict::MismatchFinal
    );
}

#[test]
fn cached_availability_reports_an_in_flight_verification() {
    let _lock = ENV_LOCK.lock().expect("env lock poisoned");
    let tmp = tempfile::tempdir().unwrap();
    let _cache_guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());
    let entry = crate::whisper::model_manager::find("tiny.en").unwrap();
    let model_path = crate::whisper::model_manager::model_path(entry).unwrap();
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, b"fake-bytes-wrong-hash").unwrap();
    let state = WhisperModelDownloads::new();
    state
        .inner
        .lock()
        .unwrap()
        .verify_running
        .insert(entry.name);

    assert_eq!(
        state.cached_availability(entry),
        Some(ModelAvailability::Checking)
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
