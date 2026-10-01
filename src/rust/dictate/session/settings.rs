//! settings for the session-owned utterance lifecycle.

use super::*;

impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
    /// Current state-machine phase. Exposed for tests and the
    /// supervisor's UI; the session itself is the source of truth.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Current recording epoch. Returned by [`Self::start`] and read by
    /// [`Self::cancel`] for the chord-race guard.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Replace the ambient live-setting overlay and apply it immediately to
    /// the current utterance's owned consumers. The current profile still wins
    /// per key; a subsequent [`Self::start`] resolves and reapplies the next
    /// utterance's profile.
    pub fn update_live_settings(&mut self, settings: std::collections::BTreeMap<String, String>) {
        self.live_settings = settings;
        self.apply_active_profile();
    }

    /// Re-set the per-session min-record floor in seconds. The
    /// `min_record_seconds` setting is `live: true` in
    /// `shared/config/settings_schema.json`; the runtime calls this at an
    /// utterance boundary so a Settings save takes effect without rebuilding
    /// the session. The skip helper still
    /// clamps the effective floor up to
    /// [`crate::dictate::skip::MIN_RECORD_FLOOR_S`] (0.3 s) regardless,
    /// so a misconfigured 0 still surfaces the misfire protection.
    pub fn update_min_record_seconds(&mut self, seconds: f64) {
        self.config.min_record_seconds = seconds;
        // Also update the base so a subsequent utterance whose profile
        // does NOT override `min_record_seconds` still sees the live-
        // reloaded value (rather than snapping back to the stale
        // construction-time floor when `apply_active_profile` resets
        // `self.config = self.base_config.clone()`). Codex-anticipated
        // guard on the profile seam introduced by
        // `rust-target-profile-matching`.
        self.base_config.min_record_seconds = seconds;
    }

    /// Update only the input label emitted by subsequent capture status
    /// events. The configured selector remains in `config` / `base_config`, so
    /// reconnect recovery can return to it without changing persisted intent.
    #[cfg(test)]
    pub(crate) fn set_effective_audio_device(&self, device: impl Into<String>) {
        *self
            .effective_audio_device
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = device.into();
    }

    /// Share only the effective-device label with the capture pump. Recovery
    /// can validate while transcription owns the outer session mutex, so this
    /// small lock must remain independent from the utterance state machine.
    #[cfg(all(
        feature = "audio-capture",
        feature = "whisper-rs-local",
        feature = "rust-injection"
    ))]
    pub(crate) fn effective_audio_device_handle(&self) -> Arc<RwLock<String>> {
        Arc::clone(&self.effective_audio_device)
    }

    /// Read-only access to the transcribe backend. Tests use this to
    /// inspect what the session passed to the mock; production callers
    /// will rarely need it.
    pub fn transcribe_backend(&self) -> &T {
        &self.transcribe
    }

    /// Read-only access to the inject backend. Tests use this to assert
    /// what the session injected.
    pub fn inject_backend(&self) -> &I {
        &self.inject
    }
}
