//! Tests for [`super::ProductionTranscribeBackend`]. Hermetic: the local
//! variant wraps a canned stub `L`; the cloud variant wraps a real
//! [`CloudTranscribeBackend`] whose empty-key guard trips BEFORE any
//! network call, so no live endpoint is contacted.

use std::cell::Cell;

use super::*;
use crate::dictate::backends::CloudTranscribeConfig;

/// A cloud config whose empty api_key trips the pre-network guard, so any
/// helper that constructs one never contacts a live endpoint.
fn cloud_config_no_key() -> CloudTranscribeConfig {
    CloudTranscribeConfig {
        base_url: "https://api.groq.com/openai/v1".to_owned(),
        api_key: String::new(),
        model: "whisper-large-v3-turbo".to_owned(),
        timeout_ms: 100,
        language: None,
        prompt: None,
    }
}

/// Canned local backend: returns a fixed transcript so a test can prove
/// the enum routed to the `Local` arm (and passed `pcm` / `sample_rate`
/// straight through).
struct StubLocal {
    text: &'static str,
}

impl TranscribeBackend for StubLocal {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        Ok(TranscribeResult {
            text: self.text.to_owned(),
            duration_s: pcm.len() as f64 / f64::from(sample_rate.max(1)),
            ..Default::default()
        })
    }
}

#[test]
fn local_variant_delegates_to_wrapped_backend() {
    let backend: ProductionTranscribeBackend<StubLocal> =
        ProductionTranscribeBackend::Local(StubLocal { text: "local hi" });
    let result = backend.transcribe(&[0.0_f32; 1600], 16_000).expect("ok");
    assert_eq!(result.text, "local hi");
    // 1600 samples / 16 kHz == 0.1 s: proves pcm + rate reached the arm.
    assert!(
        (result.duration_s - 0.1).abs() < 1e-9,
        "{}",
        result.duration_s
    );
}

/// PCM that passes the cloud backend's pre-transcription speech gate (loud
/// frames with contrast, ending loud), so the cloud arm reaches its
/// empty-key network guard rather than being short-circuited by the gate.
fn gate_passing_pcm() -> Vec<f32> {
    let mut pcm = Vec::with_capacity(6 * 480);
    for amp in [0.001_f32, 0.5, 0.001, 0.5, 0.001, 0.5] {
        pcm.extend(std::iter::repeat_n(amp, 480));
    }
    pcm
}

#[test]
fn cloud_variant_delegates_to_cloud_backend() {
    // Empty api_key trips CloudTranscribeBackend's pre-network guard, so
    // an Err proves the enum routed to the Cloud arm without a live call.
    // Feed gate-passing audio so the speech gate doesn't short-circuit first.
    let backend: ProductionTranscribeBackend<StubLocal> =
        ProductionTranscribeBackend::Cloud(CloudTranscribeBackend::new(cloud_config_no_key()));
    let err = backend
        .transcribe(&gate_passing_pcm(), 16_000)
        .expect_err("empty key must error through the cloud arm");
    assert!(matches!(err, TranscribeError::Backend(_)));
}

// ── select(): the make_real_session STT-backend choice (Codex #540) ──────────

/// The cloud path must build the `Cloud` variant AND must NOT invoke the
/// local builder -- that is the "cloud STT skips local model resolution"
/// contract (`make_real_session` puts `resolve_model_path_from_env` inside
/// the local thunk, so "local builder not called" == "no model needed").
#[test]
fn select_cloud_builds_cloud_and_skips_local_builder() {
    let local_built = Cell::new(false);
    let backend = ProductionTranscribeBackend::<StubLocal>::select(
        true,
        || Ok::<_, String>(CloudTranscribeBackend::new(cloud_config_no_key())),
        || {
            local_built.set(true);
            Ok::<_, String>(StubLocal { text: "unused" })
        },
    )
    .expect("cloud arm builds");
    assert!(matches!(backend, ProductionTranscribeBackend::Cloud(_)));
    assert!(
        !local_built.get(),
        "cloud path must NOT run the local builder (would resolve a model)"
    );
}

/// The local path (default / any non-`openai` value) must build the
/// `Local` variant and must NOT construct the cloud backend.
#[test]
fn select_local_builds_local_and_skips_cloud_builder() {
    let cloud_built = Cell::new(false);
    let backend = ProductionTranscribeBackend::<StubLocal>::select(
        false,
        || {
            cloud_built.set(true);
            Ok::<_, String>(CloudTranscribeBackend::new(cloud_config_no_key()))
        },
        || Ok::<_, String>(StubLocal { text: "local hi" }),
    )
    .expect("local built");
    match backend {
        ProductionTranscribeBackend::Local(b) => assert_eq!(
            b.transcribe(&[0.0_f32; 16], 16_000).unwrap().text,
            "local hi"
        ),
        ProductionTranscribeBackend::Cloud(_) => panic!("expected Local variant"),
    }
    assert!(
        !cloud_built.get(),
        "local path must not construct the cloud backend"
    );
}

/// A local build failure (e.g. model-path resolution error) propagates out
/// of `select` unchanged so `make_real_session` can fall back to the stub
/// session with the human-readable message.
#[test]
fn select_propagates_local_build_error() {
    let result = ProductionTranscribeBackend::<StubLocal>::select(
        false,
        || Ok::<_, String>(CloudTranscribeBackend::new(cloud_config_no_key())),
        || Err::<StubLocal, String>("model path: missing".to_owned()),
    );
    match result {
        Ok(_) => panic!("local build error must propagate, got Ok"),
        Err(e) => assert_eq!(e, "model path: missing"),
    }
}

/// A cloud-arm build failure (e.g. the local-only privacy lock refusing a
/// remote endpoint) propagates out of `select` too, so `make_real_session`
/// falls back to the stub session rather than dictating remotely.
#[test]
fn select_propagates_cloud_build_error() {
    let result = ProductionTranscribeBackend::<StubLocal>::select(
        true,
        || Err::<CloudTranscribeBackend, String>("VOICEPI_LOCAL_ONLY=1 blocks STT".to_owned()),
        || Ok::<_, String>(StubLocal { text: "unused" }),
    );
    match result {
        Ok(_) => panic!("cloud build error must propagate, got Ok"),
        Err(e) => assert!(e.contains("LOCAL_ONLY"), "{e}"),
    }
}
