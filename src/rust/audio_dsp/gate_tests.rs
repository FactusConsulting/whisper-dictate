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

// ── prepare_for_transcription: trim -> gate -> boost (Python _transcribe_detail) ─

#[test]
fn prepare_boosts_quiet_passing_audio_and_times_the_trimmed_slice() {
    // Quiet-but-contrasty speech (raw well above the -55 dBFS too-quiet floor,
    // high SNR so it passes the gate) followed by a long silent tail. prepare
    // must (a) trim the tail so duration + decoded audio come from the body,
    // and (b) boost the quiet body toward the -20 dBFS target (louder output).
    let body = frames_of(&[0.0004, 0.02, 0.0004, 0.02, 0.0004, 0.02]);
    let body_peak = body.iter().copied().fold(0.0_f32, |m, v| m.max(v.abs()));
    let mut pcm = body;
    pcm.extend(std::iter::repeat_n(0.0_f32, 40 * FRAME_SAMPLES)); // long dead tail
    let total = pcm.len();

    match prepare_for_transcription(&pcm, 16_000, &StatusThresholds::default()) {
        PreparedAudio::Decode { audio, duration_s } => {
            assert!(
                audio.len() < total,
                "the dead tail must be trimmed from the decoded audio ({} vs {total})",
                audio.len()
            );
            assert!((duration_s - audio.len() as f64 / 16_000.0).abs() < 1e-9);
            assert!(
                duration_s < total as f64 / 16_000.0,
                "duration must reflect the trimmed length, got {duration_s}"
            );
            let out_peak = audio.iter().copied().fold(0.0_f32, |m, v| m.max(v.abs()));
            assert!(
                out_peak > body_peak * 1.5,
                "quiet audio must be boosted toward target: out_peak={out_peak} body_peak={body_peak}"
            );
        }
        other => panic!("quiet contrasty speech must Decode, got {other:?}"),
    }
}

#[test]
fn prepare_reports_zero_duration_for_zero_sample_rate() {
    // A zero sample rate must yield duration_s = 0.0 (not the trimmed sample
    // count), so an inflated duration can't disable the downstream speech-rate
    // guard. Holds on both the Decode and Reject paths.
    let loud = frames_of(&[0.001, 0.5, 0.001, 0.5, 0.001, 0.5]);
    match prepare_for_transcription(&loud, 0, &StatusThresholds::default()) {
        PreparedAudio::Decode { duration_s, .. } => assert_eq!(duration_s, 0.0),
        other => panic!("expected Decode, got {other:?}"),
    }
    match prepare_for_transcription(&frames_of(&[0.0; 6]), 0, &StatusThresholds::default()) {
        PreparedAudio::Reject { duration_s, .. } => assert_eq!(duration_s, 0.0),
        other => panic!("expected Reject, got {other:?}"),
    }
}

#[test]
fn prepare_rejects_silence_with_reason() {
    match prepare_for_transcription(&frames_of(&[0.0; 6]), 16_000, &StatusThresholds::default()) {
        PreparedAudio::Reject { reason, .. } => {
            assert!(reason.contains("too quiet"), "{reason}");
        }
        other => panic!("silence must Reject, got {other:?}"),
    }
}

#[test]
fn prepare_does_not_attenuate_loud_audio_below_input() {
    // A loud passing buffer (0.5 body) is already at/above target, so the boost
    // must not amplify it into clipping nor drop it below the input — the
    // decoded audio stays a faithful (un-clipped) rendering.
    let loud = frames_of(&[0.001, 0.5, 0.001, 0.5, 0.001, 0.5]);
    match prepare_for_transcription(&loud, 16_000, &StatusThresholds::default()) {
        PreparedAudio::Decode { audio, .. } => {
            let peak = audio.iter().copied().fold(0.0_f32, |m, v| m.max(v.abs()));
            assert!(peak <= 1.0 + 1e-6, "boost must never clip: peak={peak}");
        }
        other => panic!("loud contrasty speech must Decode, got {other:?}"),
    }
}
