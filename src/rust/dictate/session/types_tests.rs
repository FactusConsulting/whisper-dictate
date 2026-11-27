use std::error::Error;

use super::TranscribeError;
use crate::audio_dsp::{capture_metrics, StatusThresholds, FRAME_SAMPLES};
use crate::model_capacity::GpuMemoryShortfall;

#[test]
fn measured_failure_keeps_its_gpu_diagnosis_and_audio_evidence() {
    let shortage = GpuMemoryShortfall {
        required_mib: 4096,
        free_mib: 512,
    };
    let mut pcm = [0.001; 4 * FRAME_SAMPLES];
    pcm[2 * FRAME_SAMPLES..].fill(0.1);
    let error = TranscribeError::GpuMemory(shortage)
        .with_audio(capture_metrics(&pcm, &StatusThresholds::default()));

    let (cause, audio) = error.cause_and_audio();
    assert!(matches!(cause, TranscribeError::GpuMemory(actual) if *actual == shortage));
    assert_eq!(error.to_string(), shortage.to_string());
    assert!(matches!(
        error.source().and_then(|source| source.downcast_ref::<TranscribeError>()),
        Some(TranscribeError::GpuMemory(actual)) if *actual == shortage
    ));
    assert!((audio.unwrap().snr_db - 40.0).abs() < 0.1);
}
