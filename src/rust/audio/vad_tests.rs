//! Smoothing-logic unit tests for [`super::SmoothedVad`]. Pulled into
//! a sibling file (rather than an inline `mod tests`) to keep
//! `vad.rs` under the codebase's 500-LOC cap; wired in via
//! `#[path = "vad_tests.rs"] mod tests;` from `vad.rs`.

#![allow(clippy::needless_range_loop)]

use super::*;
use std::f32::consts::PI;

fn silence_frame() -> Vec<f32> {
    vec![0.0; FRAME_SAMPLES]
}

fn voice_frame(amplitude: f32) -> Vec<f32> {
    // 1 kHz sine at 16 kHz = 16 samples per period.
    (0..FRAME_SAMPLES)
        .map(|i| amplitude * (2.0 * PI * (i as f32) / 16.0).sin())
        .collect()
}

fn make_vad() -> SmoothedVad {
    // Use the RMS stub so the smoothing-logic unit tests run quickly
    // and deterministically without loading the multi-MB ONNX model.
    // End-to-end coverage of the real Silero backend lives in the
    // pipeline integration test in src/rust/tests/.
    SmoothedVad::new(SileroVad::rms_stub_for_tests())
}

#[test]
fn pure_silence_never_triggers_speech_start() {
    let mut vad = make_vad();
    let mut starts = 0;
    // ~1 second of silence = 33 frames of 30 ms.
    for _ in 0..33 {
        let event = vad.feed(&silence_frame()).expect("vad feed");
        if matches!(event, VadEvent::SpeechStart(_)) {
            starts += 1;
        }
    }
    assert_eq!(starts, 0, "silence must never produce SpeechStart");
    assert!(!vad.in_speech(), "VAD must remain in non-speech state");
}

#[test]
fn loud_sine_triggers_speech_start_after_onset_debounce() {
    let mut vad = make_vad();
    let mut events = Vec::new();
    // Feed enough silence to fill the prefill ring, then voice frames.
    for _ in 0..PREFILL_FRAMES {
        events.push(vad.feed(&silence_frame()).expect("vad feed"));
    }
    // 1 s of voice → ~33 frames. SpeechStart should fire at frame
    // index ONSET_FRAMES - 1 of the voice run (i.e. the 2nd voice frame).
    for _ in 0..33 {
        events.push(vad.feed(&voice_frame(0.5)).expect("vad feed"));
    }
    let first_start = events
        .iter()
        .position(|e| matches!(e, VadEvent::SpeechStart(_)))
        .expect("SpeechStart fires somewhere in the loud-sine run");
    // Must land within the voice region (after the silence prefill).
    assert!(
        first_start >= PREFILL_FRAMES,
        "SpeechStart fired at {first_start}, before voice began",
    );
    // And it must NOT be the very first voice frame: the onset
    // debounce requires ONSET_FRAMES consecutive voice frames.
    assert!(
        first_start >= PREFILL_FRAMES + ONSET_FRAMES - 1,
        "onset debounce violated: SpeechStart at {first_start}",
    );
}

#[test]
fn speech_start_flushes_prefill_in_chronological_order() {
    let mut vad = make_vad();
    // Push twice as many tagged silence frames as the prefill ring
    // size, so we can verify that the ring (a) keeps the most recent
    // PREFILL_FRAMES and (b) returns them in chronological order on
    // SpeechStart. Each tag is small enough that the RMS stub still
    // treats the frame as silence — the per-frame contribution is on
    // the order of 1e-9.
    let total_silence = PREFILL_FRAMES * 2;
    for tag in 0..total_silence {
        let mut f = silence_frame();
        f[1] = (tag as f32) * 1e-4 + 1e-6;
        let _ = vad.feed(&f).expect("vad feed");
    }
    // Now feed voice frames until SpeechStart fires.
    let mut burst: Option<Vec<Vec<f32>>> = None;
    for _ in 0..ONSET_FRAMES + 2 {
        if let VadEvent::SpeechStart(b) = vad.feed(&voice_frame(0.5)).expect("vad feed") {
            burst = Some(b);
            break;
        }
    }
    let burst = burst.expect("SpeechStart fires within the voice run");
    // The prefill ring is "the last PREFILL_FRAMES frames seen". The
    // burst is therefore: the last (PREFILL_FRAMES - (ONSET_FRAMES-1))
    // silence frames + the (ONSET_FRAMES - 1) voice frames captured
    // during the onset debounce + the SpeechStart-triggering voice
    // frame. Total = PREFILL_FRAMES + 1.
    assert_eq!(
        burst.len(),
        PREFILL_FRAMES + 1,
        "burst should be the prefill ring plus the triggering frame",
    );
    // The leading silence portion of the burst must be in chronological
    // order — tag i+1 follows tag i for the silence prefix. The first
    // silence frame in the burst is tag `total_silence - (PREFILL_FRAMES
    // - (ONSET_FRAMES - 1))` because the onset-debounce voice frames
    // evicted that many of the oldest silence tags.
    let silence_prefix_len = PREFILL_FRAMES - (ONSET_FRAMES - 1);
    let first_silence_tag = total_silence - silence_prefix_len;
    for i in 0..silence_prefix_len {
        let expected = ((first_silence_tag + i) as f32) * 1e-4 + 1e-6;
        assert!(
            (burst[i][1] - expected).abs() < 1e-9,
            "prefill frame {i} out of order: got tag {} want {expected}",
            burst[i][1],
        );
    }
}

#[test]
fn hangover_keeps_short_pauses_inside_speech() {
    let mut vad = make_vad();
    // Drive into speech.
    for _ in 0..PREFILL_FRAMES {
        let _ = vad.feed(&silence_frame()).expect("vad feed");
    }
    for _ in 0..ONSET_FRAMES + 1 {
        let _ = vad.feed(&voice_frame(0.5)).expect("vad feed");
    }
    assert!(vad.in_speech());
    // Feed a short silence pause (less than HANGOVER_FRAMES) — must
    // NOT end speech.
    for _ in 0..(HANGOVER_FRAMES / 2) {
        let event = vad.feed(&silence_frame()).expect("vad feed");
        assert!(
            !matches!(event, VadEvent::SpeechEnd),
            "short pause must not end speech"
        );
    }
    assert!(vad.in_speech());
    // Now feed beyond the hangover budget — SpeechEnd must fire.
    let mut ended = false;
    for _ in 0..HANGOVER_FRAMES + 2 {
        if matches!(
            vad.feed(&silence_frame()).expect("vad feed"),
            VadEvent::SpeechEnd
        ) {
            ended = true;
            break;
        }
    }
    assert!(ended, "long pause must produce SpeechEnd");
    assert!(!vad.in_speech());
}

#[test]
fn reset_during_silence_returns_false() {
    let mut vad = make_vad();
    // Pure silence — never in speech.
    for _ in 0..10 {
        let _ = vad.feed(&silence_frame()).expect("vad feed");
    }
    assert!(!vad.in_speech());
    let was_in_speech = vad.reset();
    assert!(
        !was_in_speech,
        "reset() during silence must return false (no Cancelled to emit)"
    );
}

#[test]
fn reset_during_speech_returns_true() {
    let mut vad = make_vad();
    // Drive into speech.
    for _ in 0..PREFILL_FRAMES {
        let _ = vad.feed(&silence_frame()).expect("vad feed");
    }
    for _ in 0..ONSET_FRAMES + 1 {
        let _ = vad.feed(&voice_frame(0.5)).expect("vad feed");
    }
    assert!(vad.in_speech(), "should be in speech now");
    let was_in_speech = vad.reset();
    assert!(
        was_in_speech,
        "reset() during speech must return true so caller emits Cancelled",
    );
    assert!(!vad.in_speech(), "reset clears in_speech");
}
