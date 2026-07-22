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

// ── gate_and_trim: the trimmed buffer callers decode + time ───────────────────

#[test]
fn gate_and_trim_drops_the_trailing_silence_from_the_decoded_slice() {
    // Speech body (contrasty) followed by a long silent tail. The gate must
    // pass (there IS speech), AND the returned slice must be the trimmed
    // body -- NOT the original -- so the backend decodes only the speech and
    // derives duration from the speech length. This is the parity fix for
    // the Codex finding: decoding/timing the untrimmed tail would feed
    // Whisper empty audio to hallucinate a caption over and inflate the
    // speech-rate denominator.
    let mut pcm = frames_of(&[0.001, 0.5, 0.001, 0.5, 0.001, 0.5]);
    let body_len = pcm.len();
    pcm.extend(std::iter::repeat_n(0.0_f32, 40 * FRAME_SAMPLES)); // long dead tail

    let gated = gate_and_trim(&pcm, &StatusThresholds::default());
    assert!(gated.reject.is_none(), "audio with a speech body must pass");
    assert!(
        gated.trimmed.len() < pcm.len(),
        "the dead tail must be trimmed off the decoded slice ({} vs {})",
        gated.trimmed.len(),
        pcm.len()
    );
    // The trim keeps the speech body plus the trimmer's short pad, so nearly
    // all of the 40-frame dead tail is removed from what the backend decodes +
    // times. Assert the bulk of the tail is gone (robust to the exact pad):
    // that shrunken length is the speech-rate guard's denominator, and the
    // audio the model never sees empty tail to hallucinate a caption over.
    let trimmed_away = pcm.len() - gated.trimmed.len();
    assert!(
        trimmed_away >= 30 * FRAME_SAMPLES,
        "most of the 40-frame dead tail must be trimmed; only {trimmed_away} \
         samples removed (body={body_len}, total={})",
        pcm.len()
    );
}

#[test]
fn gate_and_trim_reports_reject_reason_for_silence() {
    let silence = frames_of(&[0.0; 6]);
    let gated = gate_and_trim(&silence, &StatusThresholds::default());
    let reason = gated.reject.expect("silence must be rejected");
    assert!(reason.contains("too quiet"), "{reason}");
}
