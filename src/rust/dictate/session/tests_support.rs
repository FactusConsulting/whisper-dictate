//! Test backends + helpers shared across the `tests_*` files in this
//! module. Kept tiny on purpose: no external dep, no builder DSL —
//! everything is a `RefCell` you read in the assertion.

use std::cell::RefCell;

use serde_json::Value;

use super::{
    DictateSession, InjectBackend, InjectError, PostProcessBackend, PostProcessOutcome,
    SessionConfig, TranscribeBackend, TranscribeError, TranscribeResult, SR,
};

// ── test backends ────────────────────────────────────────────────────────────

/// Controllable transcribe mock. Set `next` to drive the next
/// `transcribe()` call's outcome; the call argument is captured into
/// `seen_pcm_len` so tests can assert resample / channel-select
/// correctness without depending on the audio crate.
pub(super) struct TestTranscribe {
    pub(super) next: RefCell<TranscribeOutcome>,
    pub(super) seen_pcm_len: RefCell<Vec<usize>>,
    pub(super) seen_sample_rate: RefCell<Vec<u32>>,
}

#[derive(Clone)]
pub(super) enum TranscribeOutcome {
    Ok(TranscribeResult),
    Err(String),
}

impl TestTranscribe {
    pub(super) fn returning_text(text: &str) -> Self {
        Self {
            next: RefCell::new(TranscribeOutcome::Ok(TranscribeResult {
                text: text.into(),
                is_hallucination: false,
                latency_ms: 42,
                duration_s: 1.23,
                language: "en".into(),
                gate: None,
            })),
            seen_pcm_len: RefCell::new(Vec::new()),
            seen_sample_rate: RefCell::new(Vec::new()),
        }
    }

    pub(super) fn returning_hallucination(text: &str) -> Self {
        let t = Self::returning_text(text);
        *t.next.borrow_mut() = TranscribeOutcome::Ok(TranscribeResult {
            text: text.into(),
            is_hallucination: true,
            latency_ms: 7,
            duration_s: 0.4,
            language: "en".into(),
            gate: None,
        });
        t
    }

    pub(super) fn returning_empty() -> Self {
        let t = Self::returning_text("");
        *t.next.borrow_mut() = TranscribeOutcome::Ok(TranscribeResult::default());
        t
    }

    pub(super) fn returning_error(msg: &str) -> Self {
        Self {
            next: RefCell::new(TranscribeOutcome::Err(msg.into())),
            seen_pcm_len: RefCell::new(Vec::new()),
            seen_sample_rate: RefCell::new(Vec::new()),
        }
    }
}

impl TranscribeBackend for TestTranscribe {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        self.seen_pcm_len.borrow_mut().push(pcm.len());
        self.seen_sample_rate.borrow_mut().push(sample_rate);
        match self.next.borrow().clone() {
            TranscribeOutcome::Ok(result) => Ok(result),
            TranscribeOutcome::Err(msg) => Err(TranscribeError::Backend(msg)),
        }
    }
}

/// Inject mock: captures every text passed to `inject()`. `fail_with`
/// makes the next call (and only the next) return an error so tests can
/// drive the inject-failure branch without a builder dance.
pub(super) struct TestInject {
    pub(super) injected: RefCell<Vec<String>>,
    pub(super) fail_with: RefCell<Option<String>>,
}

impl TestInject {
    pub(super) fn new() -> Self {
        Self {
            injected: RefCell::new(Vec::new()),
            fail_with: RefCell::new(None),
        }
    }

    /// Arm the mock so the next `inject()` call fails with `msg`.
    pub(super) fn failing(msg: &str) -> Self {
        let i = Self::new();
        *i.fail_with.borrow_mut() = Some(msg.into());
        i
    }
}

impl InjectBackend for TestInject {
    fn inject(&self, text: &str) -> Result<(), InjectError> {
        if let Some(err) = self.fail_with.borrow_mut().take() {
            return Err(InjectError::Backend(err));
        }
        self.injected.borrow_mut().push(text.into());
        Ok(())
    }
}

/// Post-process mock: rewrites every input to a fixed `output` so tests
/// can assert the pass ran AND ran in the right order relative to the
/// format-command layer (the injected text is `format(post_process(raw))`,
/// which pins the `postprocess -> format` order without needing to peek
/// inside the boxed backend).
pub(super) struct TestPostProcess {
    output: String,
}

impl TestPostProcess {
    /// Rewrite every input to `output`.
    pub(super) fn returning(output: &str) -> Self {
        Self {
            output: output.to_owned(),
        }
    }
}

impl PostProcessBackend for TestPostProcess {
    fn post_process(&self, text: &str) -> PostProcessOutcome {
        PostProcessOutcome {
            text: self.output.clone(),
            processor: "ollama".to_owned(),
            mode: "clean".to_owned(),
            model: "test-model".to_owned(),
            latency_ms: 12,
            changed: self.output != text,
            fallback: false,
            error: String::new(),
            redacted: false,
            redactions: Vec::new(),
        }
    }
}

// ── small helpers ─────────────────────────────────────────────────────────────

/// One second of silent 16 kHz mono PCM. Long enough to clear the
/// 0.5 s default min-record floor.
pub(super) fn one_second_pcm() -> Vec<f32> {
    vec![0.0_f32; SR as usize]
}

/// Build a session with the given backends + the default config (0.5 s
/// min-record floor, blank capture/device labels). Returns
/// `(session, byte_buf, env_lock_guard)`; the guard MUST be bound (e.g.
/// `let (mut s, mut buf, _guard) = session(...)`) so it lives for the
/// rest of the test -- it serialises `VOICEPI_WORKER_EVENTS` mutations
/// against `events_tests` which mutates the same variable in the same
/// library test binary.
///
/// Codex P2 #413 round 3 (tests_support.rs:152): the previous round set
/// the env var without taking the crate-wide `test_env_lock::ENV_LOCK`,
/// which races against `events_tests::worker_events_env_var_gates_emission`
/// and any other parallel test that observes the variable. Under the
/// Rust 2024 edition `set_var` is `unsafe` precisely because of this
/// hazard; the single crate-wide lock is the only sound design.
pub(super) fn session<T: TranscribeBackend, I: InjectBackend>(
    transcribe: T,
    inject: I,
) -> (
    DictateSession<T, I>,
    Vec<u8>,
    std::sync::MutexGuard<'static, ()>,
) {
    session_with_config(transcribe, inject, SessionConfig::default())
}

/// Same as [`session`] but with a caller-supplied [`SessionConfig`], so
/// a test can drive a non-default field (e.g. `format_command_set`)
/// without duplicating the `ENV_LOCK` + `VOICEPI_WORKER_EVENTS` dance.
pub(super) fn session_with_config<T: TranscribeBackend, I: InjectBackend>(
    transcribe: T,
    inject: I,
    config: SessionConfig,
) -> (
    DictateSession<T, I>,
    Vec<u8>,
    std::sync::MutexGuard<'static, ()>,
) {
    let guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("VOICEPI_WORKER_EVENTS", "1");
    (
        DictateSession::new(transcribe, inject, config),
        Vec::new(),
        guard,
    )
}

/// Parse the captured `[worker-event] {...}\n` lines into JSON values.
/// Matches the Python test helper in `test_dictate_loop.py`'s
/// `_run_capture_worker_events`.
pub(super) fn parse_events(bytes: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(bytes).expect("event stream must be UTF-8");
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

/// All emitted `state` strings, in order. Lets a test eyeball the
/// transition shape without spelling the full event payloads out.
pub(super) fn state_trace(bytes: &[u8]) -> Vec<String> {
    parse_events(bytes)
        .into_iter()
        .filter_map(|e| {
            e.get("state")
                .and_then(|s| s.as_str())
                .map(|s| s.to_owned())
        })
        .collect()
}

/// Drive a session from idle through start → push frame → stop and
/// return (final outcome, captured event bytes, the session). Saves a
/// few lines of boilerplate in every happy-path test.
///
/// Note: this overload takes an already-constructed session, so callers
/// must have bound the `_guard` from a prior `session(...)` call; that
/// guard already holds `ENV_LOCK` for the whole test scope.
pub(super) fn run_one_utterance<T: TranscribeBackend, I: InjectBackend>(
    mut s: DictateSession<T, I>,
    pcm: &[f32],
) -> (super::UtteranceOutcome, Vec<u8>, DictateSession<T, I>) {
    let mut buf = Vec::new();
    s.start(&mut buf).expect("start");
    s.push_frame(pcm);
    let outcome = s
        .stop_and_transcribe(&mut buf)
        .expect("stop_and_transcribe");
    (outcome, buf, s)
}
