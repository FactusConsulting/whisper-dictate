//! Nonblocking verification cache and background checksum jobs.

use super::*;

impl WhisperModelDownloads {
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
        self.availability_fast(entry) == ModelAvailability::Available
    }

    /// Return the cached model's availability and schedule verification when
    /// needed. A present file is `Checking` until its checksum is known.
    pub fn availability_fast(
        &self,
        entry: &'static crate::whisper::model_manager::ModelEntry,
    ) -> ModelAvailability {
        let Some((mtime, len)) = model_metadata(entry) else {
            self.clear_verification(entry.name);
            return ModelAvailability::Missing;
        };
        if let Some(availability) = self.cached_or_running_availability(entry, mtime, len) {
            return availability;
        }
        if self.spawn_verification(entry) {
            ModelAvailability::Checking
        } else {
            ModelAvailability::Missing
        }
    }

    /// Return a cached verification result without starting a checksum.
    /// Runtime startup uses this to avoid hashing the selected model twice.
    pub fn cached_availability(
        &self,
        entry: &'static crate::whisper::model_manager::ModelEntry,
    ) -> Option<ModelAvailability> {
        let (mtime, len) = model_metadata(entry)?;
        let inner = self.inner.lock().ok()?;
        if inner.verify_running.contains(entry.name) {
            return Some(ModelAvailability::Checking);
        }
        let cached = inner.verify_cache.get(entry.name)?;
        if cached.verdict == VerificationVerdict::RetryableError
            && cached.checked_at.elapsed() >= FAILED_VERIFICATION_RETRY_AFTER
        {
            return None;
        }
        if matches!(
            cached.verdict,
            VerificationVerdict::Mismatch | VerificationVerdict::MismatchFinal
        ) && cached.checked_at.elapsed() >= FAILED_VERIFICATION_RETRY_AFTER
            && model_fingerprint(entry) != cached.fingerprint
        {
            return None;
        }
        (cached.mtime == mtime && cached.len == len).then_some({
            match cached.verdict {
                VerificationVerdict::Verified => ModelAvailability::Available,
                VerificationVerdict::Mismatch
                | VerificationVerdict::MismatchFinal
                | VerificationVerdict::MismatchRetryPending
                | VerificationVerdict::RetryableError => ModelAvailability::Missing,
            }
        })
    }

    pub(super) fn clear_verification(&self, name: &'static str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.verify_cache.remove(name);
            inner.verify_running.remove(name);
        }
    }

    pub(super) fn cached_or_running_availability(
        &self,
        entry: &'static crate::whisper::model_manager::ModelEntry,
        mtime: SystemTime,
        len: u64,
    ) -> Option<ModelAvailability> {
        let name = entry.name;
        let Ok(mut inner) = self.inner.lock() else {
            return Some(ModelAvailability::Checking);
        };
        let mut clear_retryable = false;
        if let Some(cached) = inner.verify_cache.get_mut(name) {
            if cached.mtime == mtime && cached.len == len {
                match cached.verdict {
                    VerificationVerdict::Verified => {
                        return Some(ModelAvailability::Available);
                    }
                    VerificationVerdict::Mismatch
                        if cached.checked_at.elapsed() >= FAILED_VERIFICATION_RETRY_AFTER =>
                    {
                        cached.verdict = VerificationVerdict::MismatchRetryPending;
                    }
                    VerificationVerdict::MismatchFinal
                        if cached.checked_at.elapsed() >= FAILED_VERIFICATION_RETRY_AFTER =>
                    {
                        if model_fingerprint(entry) != cached.fingerprint {
                            cached.verdict = VerificationVerdict::MismatchRetryPending;
                        } else {
                            cached.checked_at = Instant::now();
                            return Some(ModelAvailability::Missing);
                        }
                    }
                    VerificationVerdict::RetryableError
                        if cached.checked_at.elapsed() >= FAILED_VERIFICATION_RETRY_AFTER =>
                    {
                        clear_retryable = true;
                    }
                    VerificationVerdict::Mismatch
                    | VerificationVerdict::MismatchFinal
                    | VerificationVerdict::MismatchRetryPending
                    | VerificationVerdict::RetryableError => {
                        return Some(ModelAvailability::Missing);
                    }
                }
            }
        }
        if clear_retryable {
            inner.verify_cache.remove(name);
        }
        (!inner.verify_running.insert(name)).then_some(ModelAvailability::Checking)
    }

    pub(super) fn spawn_verification(
        &self,
        entry: &'static crate::whisper::model_manager::ModelEntry,
    ) -> bool {
        let state = self.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("whisper-verify-{}", entry.name))
            .spawn(move || {
                state.finish_verification(entry, verified_model_metadata(entry));
            })
            .is_ok();
        if !spawned {
            self.clear_verification(entry.name);
        }
        spawned
    }

    pub(super) fn finish_verification(
        &self,
        entry: &'static crate::whisper::model_manager::ModelEntry,
        result: Option<(SystemTime, u64, VerificationVerdict, Option<u64>)>,
    ) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if let Some((mtime, len, verdict, fingerprint)) = result {
            let verdict = if verdict == VerificationVerdict::Mismatch
                && inner.verify_cache.get(entry.name).is_some_and(|cached| {
                    cached.verdict == VerificationVerdict::MismatchRetryPending
                }) {
                VerificationVerdict::MismatchFinal
            } else {
                verdict
            };
            inner.verify_cache.insert(
                entry.name,
                VerifyCacheEntry {
                    mtime,
                    len,
                    verdict,
                    fingerprint,
                    checked_at: Instant::now(),
                },
            );
        }
        inner.verify_running.remove(entry.name);
    }
}

pub(super) fn model_metadata(
    entry: &'static crate::whisper::model_manager::ModelEntry,
) -> Option<(SystemTime, u64)> {
    let path = crate::whisper::model_manager::model_path(entry).ok()?;
    let metadata = path.is_file().then(|| path.metadata().ok())??;
    Some((metadata.modified().ok()?, metadata.len()))
}

const FINGERPRINT_SAMPLE_BYTES: usize = 4096;

pub(super) fn model_fingerprint(
    entry: &'static crate::whisper::model_manager::ModelEntry,
) -> Option<u64> {
    let path = crate::whisper::model_manager::model_path(entry).ok()?;
    file_fingerprint(&path)
}

pub(super) fn file_fingerprint(path: &Path) -> Option<u64> {
    let metadata = path.metadata().ok()?;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = DefaultHasher::new();
    metadata.len().hash(&mut hasher);

    let mut sample = [0u8; FINGERPRINT_SAMPLE_BYTES];
    let first = file.read(&mut sample).ok()?;
    first.hash(&mut hasher);
    sample[..first].hash(&mut hasher);
    if metadata.len() > FINGERPRINT_SAMPLE_BYTES as u64 {
        file.seek(SeekFrom::End(-(FINGERPRINT_SAMPLE_BYTES as i64)))
            .ok()?;
        let last = file.read(&mut sample).ok()?;
        last.hash(&mut hasher);
        sample[..last].hash(&mut hasher);
    }
    Some(hasher.finish())
}

pub(super) fn verified_model_metadata(
    entry: &'static crate::whisper::model_manager::ModelEntry,
) -> Option<(SystemTime, u64, VerificationVerdict, Option<u64>)> {
    let before = model_metadata(entry)?;
    let verdict = match crate::whisper::model_manager::model_path(entry) {
        Ok(path) => match crate::whisper::model_manager::verify_sha256(&path, entry.sha256) {
            Ok(()) => VerificationVerdict::Verified,
            Err(error) if error.to_string().starts_with("SHA-256 mismatch for ") => {
                VerificationVerdict::Mismatch
            }
            Err(_) => VerificationVerdict::RetryableError,
        },
        Err(_) => VerificationVerdict::RetryableError,
    };
    let after = model_metadata(entry)?;
    let fingerprint = model_fingerprint(entry);
    (before == after).then_some((after.0, after.1, verdict, fingerprint))
}
