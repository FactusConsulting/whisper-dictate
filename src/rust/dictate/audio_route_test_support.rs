//! Test backends + helpers shared by [`super::audio_route_tests`].
//!
//! Kept tiny on purpose — no external crate, no builder DSL — and split
//! from the test file so the main file stays under the 500 LOC bar set
//! in AGENTS.md while keeping the test setup discoverable per the
//! pattern in `session/tests_support.rs`.

use std::cell::RefCell;
use std::io::Write;

use serde_json::Value;

use crate::dictate::audio_route::{AudioRoute, RouteConfig};
use crate::dictate::session::{
    DictateSession, InjectBackend, InjectError, SessionConfig, TranscribeBackend, TranscribeError,
    TranscribeResult,
};

// ── test backends ────────────────────────────────────────────────────────────

/// Minimal transcribe backend: returns a fixed non-empty text so the
/// session's `stop_and_transcribe` resolves to `Injected` and we can
/// observe the buffered sample count handed to the model.
pub(super) struct TestTranscribe {
    pub(super) seen_pcm_len: RefCell<Vec<usize>>,
}

impl TestTranscribe {
    pub(super) fn new() -> Self {
        Self {
            seen_pcm_len: RefCell::new(Vec::new()),
        }
    }
}

impl TranscribeBackend for TestTranscribe {
    fn transcribe(
        &self,
        pcm: &[f32],
        _sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        self.seen_pcm_len.borrow_mut().push(pcm.len());
        Ok(TranscribeResult {
            text: "hello".into(),
            ..TranscribeResult::default()
        })
    }
}

/// Minimal inject backend: stash every injected string so the test can
/// assert end-to-end the cap-trip-then-release path actually produced
/// an utterance.
pub(super) struct TestInject {
    pub(super) injected: RefCell<Vec<String>>,
}

impl TestInject {
    pub(super) fn new() -> Self {
        Self {
            injected: RefCell::new(Vec::new()),
        }
    }
}

impl InjectBackend for TestInject {
    fn inject(&self, text: &str) -> Result<(), InjectError> {
        self.injected.borrow_mut().push(text.into());
        Ok(())
    }
}

// ── env helper ───────────────────────────────────────────────────────────────

// Re-export the crate-wide `EnvVarGuard` so callers in this module can keep
// the historical `EnvVarGuard` name (the audio_route tests + the
// `start_recording_with_cap_env` helper below). Callers MUST hold the
// crate-wide [`crate::test_env_lock::ENV_LOCK`] for the guard's lifetime
// — see `crate::test_env_lock` for the soundness contract.
pub(super) use crate::test_env_lock::EnvVarGuard;

// ── route builders ───────────────────────────────────────────────────────────

/// Build a route with the given cap, the default capture-backend
/// labels, and a min-record floor of `0.0`. The 0.3 s misfire floor
/// in `crate::dictate::skip` still clamps the effective floor up to
/// 0.3 s, so tests that exercise the transcribe path use a buffer
/// above the floor (the 0.36 s cap-trip companion test).
pub(super) fn route_with_cap(cap_seconds: Option<f64>) -> AudioRoute<TestTranscribe, TestInject> {
    let session = DictateSession::new(
        TestTranscribe::new(),
        TestInject::new(),
        SessionConfig {
            min_record_seconds: 0.0,
            ..SessionConfig::default()
        },
    );
    AudioRoute::for_test(
        session,
        RouteConfig {
            max_record_seconds: cap_seconds,
            ..RouteConfig::default()
        },
    )
}

/// Variant that pins `VOICEPI_MAX_RECORD_S` to the test's cap so the
/// env-refresh in `start_recording` (Codex P2 #415 audio_route.rs:250)
/// doesn't overwrite the cap mid-test. Tests that don't care about
/// the cap can use [`route_with_cap`] with `None` and additionally
/// clear the env var via an [`EnvVarGuard::set("…", "0")`] so the
/// refresh lands on `None`.
///
/// Returns `(recording_id, EnvVarGuard)`. Callers MUST bind the guard
/// (e.g. `let (_id, _cap_guard) = start_recording_with_cap_env(...)`)
/// so the previous `VOICEPI_MAX_RECORD_S` value is restored at end of
/// scope — without the guard, any later library test in the same
/// process inherits the cap from whichever audio-route test ran last
/// and the suite goes order-dependent. Codex P2 #415
/// test_support.rs:143.
pub(super) fn start_recording_with_cap_env<W: Write>(
    route: &mut AudioRoute<TestTranscribe, TestInject>,
    writer: &mut W,
    cap_seconds: f64,
) -> (u64, EnvVarGuard) {
    let guard = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", cap_seconds.to_string());
    let id = route.start_recording(writer).expect("start_recording");
    (id, guard)
}

// ── wire helpers ─────────────────────────────────────────────────────────────

/// Parse the captured `[worker-event] {…}\n` lines into JSON values.
/// Mirrors the equivalent helper in `session/tests_support.rs`.
pub(super) fn parse_events(bytes: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(bytes).expect("event stream is UTF-8");
    let mut events = Vec::new();
    for line in text.lines() {
        if let Some(payload) = line.strip_prefix("[worker-event] ") {
            events.push(
                serde_json::from_str(payload).expect("worker-event payload must be valid JSON"),
            );
        }
    }
    events
}
