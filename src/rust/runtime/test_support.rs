use super::*;
use std::ffi::{OsStr, OsString};
use std::sync::{Arc, Mutex};

use crate::dictate::{
    DictateSession, InjectBackend, InjectError, SessionConfig, TranscribeBackend, TranscribeError,
    TranscribeResult,
};

pub(super) struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(key);
        env::set_var(key, value);
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            env::set_var(self.key, value);
        } else {
            env::remove_var(self.key);
        }
    }
}

// ── stub session backends ────────────────────────────────────────────────────

/// Transcribe backend that returns empty text with a stub gate string, so
/// a session driven by the coordinator sink takes the `no_text` branch and
/// emits the matching worker event without whisper.cpp. The gate string
/// passes through [`crate::dictate::session::normalize_gate_reason`] and
/// lands as `reason="empty"` on the emitted event (the normalizer matches
/// on `"too quiet"` / `"no speech"` substrings only).
#[derive(Debug, Default)]
pub(super) struct StubTranscribe;

impl TranscribeBackend for StubTranscribe {
    fn transcribe(
        &self,
        _pcm: &[f32],
        _sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        Ok(TranscribeResult {
            text: String::new(),
            gate: Some("rust-session-test-stub".to_owned()),
            ..Default::default()
        })
    }
}

/// Inject backend that accepts every text and touches nothing.
#[derive(Debug, Default)]
pub(super) struct StubInject;

impl InjectBackend for StubInject {
    fn inject(&self, _text: &str) -> Result<(), InjectError> {
        Ok(())
    }
}

/// Session type the stub-backed sink tests drive.
pub(super) type StubSession = DictateSession<StubTranscribe, StubInject>;

/// Fresh stub-backed session with worker events enabled, wrapped in
/// `Arc<Mutex<…>>` so a test can hold a clone for direct `push_frame`
/// access while the action sink owns its own clone.
pub(super) fn stub_session() -> Arc<Mutex<StubSession>> {
    Arc::new(Mutex::new(
        DictateSession::new(StubTranscribe, StubInject, SessionConfig::default())
            .with_worker_events_enabled(),
    ))
}
