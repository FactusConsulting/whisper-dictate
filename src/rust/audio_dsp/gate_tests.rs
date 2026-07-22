//! Tests for the env-threshold reader + [`super::speech_gate_reason`]. All
//! hermetic (synthetic PCM); the DSP formulas themselves are pinned by
//! `metrics.rs`'s own characterisation suite.

use super::*;

/// Build `frames` frames of [`FRAME_SAMPLES`] samples each, every sample in
/// a frame set to that frame's amplitude.
fn frames_of(amplitudes: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(amplitudes.len() * FRAME_SAMPLES);
    for &amp in amplitudes {
        out.extend(std::iter::repeat_n(amp, FRAME_SAMPLES));
    }
    out
}

fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: std::collections::HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name: &str| map.get(name).cloned()
}

#[test]
fn thresholds_default_when_unset() {
    let t = thresholds_from_env_with(lookup_from(&[]));
    assert_eq!(t.target_dbfs, DEFAULT_TARGET_DBFS);
    assert_eq!(t.min_input_dbfs, DEFAULT_MIN_INPUT_DBFS);
    assert_eq!(t.min_input_snr_db, DEFAULT_MIN_INPUT_SNR_DB);
}

#[test]
fn thresholds_parse_overrides_and_ignore_junk() {
    let t = thresholds_from_env_with(lookup_from(&[
        (TARGET_DBFS_ENV, "-16"),
        (MIN_INPUT_DBFS_ENV, "-50"),
        (MIN_SNR_DB_ENV, "9"),
    ]));
    assert_eq!(t.target_dbfs, -16.0);
    assert_eq!(t.min_input_dbfs, -50.0);
    assert_eq!(t.min_input_snr_db, 9.0);

    // Unparseable / blank falls back to the default.
    let bad = thresholds_from_env_with(lookup_from(&[(MIN_SNR_DB_ENV, "nope")]));
    assert_eq!(bad.min_input_snr_db, DEFAULT_MIN_INPUT_SNR_DB);
}

#[test]
fn gate_rejects_silence_as_too_quiet() {
    let silence = frames_of(&[0.0; 6]);
    let reason =
        speech_gate_reason(&silence, &StatusThresholds::default()).expect("silence must be gated");
    assert!(reason.contains("too quiet"), "{reason}");
    // The session's mapper keys off this substring.
    assert_eq!(
        crate::dictate::session::normalize_gate_reason(&reason),
        "too_quiet"
    );
}

#[test]
fn gate_rejects_flat_loud_tone_as_no_contrast() {
    // Loud but no frame-to-frame contrast -> passes the too-quiet floor,
    // fails the SNR floor.
    let flat = frames_of(&[0.5; 6]);
    let reason =
        speech_gate_reason(&flat, &StatusThresholds::default()).expect("flat tone must be gated");
    assert!(reason.contains("no speech contrast"), "{reason}");
    assert_eq!(
        crate::dictate::session::normalize_gate_reason(&reason),
        "no_speech"
    );
}

#[test]
fn gate_passes_loud_audio_with_contrast() {
    // Alternating quiet/loud frames, ending LOUD so trailing-silence trim
    // keeps the contrast: high SNR + healthy level -> looks like speech.
    let speechy = frames_of(&[0.001, 0.5, 0.001, 0.5, 0.001, 0.5]);
    assert!(
        speech_gate_reason(&speechy, &StatusThresholds::default()).is_none(),
        "loud contrasty audio must pass the gate"
    );
}
