//! Tests for [`super::ProductionTranscribeBackend`]. Hermetic: the local
//! variant wraps a canned stub `L`; the cloud variant wraps a real
//! [`CloudTranscribeBackend`] whose empty-key guard trips BEFORE any
//! network call, so no live endpoint is contacted.

use super::*;
use crate::dictate::backends::CloudTranscribeConfig;

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

#[test]
fn cloud_variant_delegates_to_cloud_backend() {
    // Empty api_key trips CloudTranscribeBackend's pre-network guard, so
    // an Err proves the enum routed to the Cloud arm without a live call.
    let cloud = CloudTranscribeBackend::new(CloudTranscribeConfig {
        base_url: "https://api.groq.com/openai/v1".to_owned(),
        api_key: String::new(),
        model: "whisper-large-v3-turbo".to_owned(),
        timeout_ms: 100,
        language: None,
        prompt: None,
    });
    let backend: ProductionTranscribeBackend<StubLocal> = ProductionTranscribeBackend::Cloud(cloud);
    let err = backend
        .transcribe(&[0.0_f32; 1600], 16_000)
        .expect_err("empty key must error through the cloud arm");
    assert!(matches!(err, TranscribeError::Backend(_)));
}
