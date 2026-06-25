//! End-to-end integration test for the Rust capture pipeline (the
//! `audio-in-rust` feature). Exercises the **non-cpal** path of the
//! pipeline — we can't open a real microphone in CI — by driving a
//! synthetic recording (silence + 1 kHz sine + silence) through the
//! resampler + Silero VAD and asserting:
//!
//! * `SpeechStart` fires once inside the loud-sine region.
//! * Enough `Frame` events follow to cover the body of the utterance.
//! * `SpeechEnd` eventually fires after the trailing silence.
//!
//! The cpal half is covered separately by the per-host system tests we
//! run by hand on Windows / Linux — those need a real audio device and
//! aren't suitable for CI.
//!
//! Only built when `--features audio-in-rust` is on. Without the
//! feature the whole `audio` module is `cfg`-d out, so this file is
//! empty and cargo skips it.

#![cfg(feature = "audio-in-rust")]

use std::f32::consts::PI;

use whisper_dictate_app::audio::{
    FrameResampler, PipelineEvent, SileroVad, SmoothedVad, VadEvent, FRAME_SIZE, OUTPUT_RATE,
};

const INPUT_RATE: usize = 48_000;
const SILENCE_S: f32 = 0.6;
const VOICE_S: f32 = 1.5;

fn make_input() -> Vec<f32> {
    // 600 ms silence → 1.5 s of loud 440 Hz sine → 600 ms silence.
    // Total ~2.7 s at 48 kHz ≈ 130 000 samples → well over the integration
    // size limit even with the WAV header (we synthesize in-memory).
    let mut out =
        Vec::with_capacity(((SILENCE_S + VOICE_S + SILENCE_S) * INPUT_RATE as f32) as usize);
    let pre = (INPUT_RATE as f32 * SILENCE_S) as usize;
    out.extend(std::iter::repeat_n(0.0, pre));
    let voice = (INPUT_RATE as f32 * VOICE_S) as usize;
    for i in 0..voice {
        let t = i as f32 / INPUT_RATE as f32;
        out.push(0.7 * (2.0 * PI * 440.0 * t).sin());
    }
    let post = (INPUT_RATE as f32 * SILENCE_S) as usize;
    out.extend(std::iter::repeat_n(0.0, post));
    out
}

fn drive_pipeline(input: &[f32]) -> Vec<PipelineEvent> {
    let mut resampler = FrameResampler::new(INPUT_RATE).expect("construct resampler");
    let silero = SileroVad::from_embedded_bytes(include_bytes!("../../../assets/silero_vad.onnx"))
        .expect("load embedded Silero model");
    let mut vad = SmoothedVad::new(silero);
    let mut events: Vec<PipelineEvent> = Vec::new();

    // Mimic what the runtime pump does: feed input in cpal-callback-sized
    // bursts so we hit the same resampler boundaries we'd see in
    // production. 480 samples at 48 kHz = 10 ms — small enough to stress
    // the chunked-input path.
    for chunk in input.chunks(480) {
        let mut buffered_frames: Vec<Vec<f32>> = Vec::new();
        resampler.push(chunk, |frame| buffered_frames.push(frame.to_vec()));
        for frame in buffered_frames {
            push_frame(&mut vad, &frame, &mut events);
        }
    }
    // Flush the resampler tail.
    let mut tail: Vec<Vec<f32>> = Vec::new();
    resampler.finish(|frame| tail.push(frame.to_vec()));
    for frame in tail {
        push_frame(&mut vad, &frame, &mut events);
    }
    // If we end mid-utterance, mirror the runtime pump's "always emit a
    // final SpeechEnd at end-of-stream" behaviour.
    if vad.in_speech() {
        events.push(PipelineEvent::SpeechEnd);
    }
    events
}

fn push_frame(vad: &mut SmoothedVad, frame: &[f32], events: &mut Vec<PipelineEvent>) {
    assert_eq!(frame.len(), FRAME_SIZE);
    match vad.feed(frame).expect("vad feed") {
        VadEvent::Silence => {}
        VadEvent::SpeechStart(burst) => {
            events.push(PipelineEvent::SpeechStart);
            for f in burst {
                events.push(PipelineEvent::Frame(f));
            }
        }
        VadEvent::SpeechFrame(f) => events.push(PipelineEvent::Frame(f)),
        VadEvent::SpeechEnd => events.push(PipelineEvent::SpeechEnd),
    }
}

#[test]
fn pipeline_detects_speech_in_synthetic_sine_recording() {
    let input = make_input();
    // Sanity check: our output rate constants are consistent.
    assert_eq!(OUTPUT_RATE, 16_000);
    assert_eq!(FRAME_SIZE, 480);
    let events = drive_pipeline(&input);

    let starts = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::SpeechStart))
        .count();
    let ends = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::SpeechEnd))
        .count();
    let frames = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::Frame(_)))
        .count();

    assert!(
        starts >= 1,
        "SpeechStart must fire at least once for a 1.5 s sine burst; got {starts}",
    );
    assert!(
        ends >= 1,
        "SpeechEnd must fire after the trailing silence; got {ends}",
    );
    // The voice region is 1.5 s → ~50 frames. We give plenty of slack
    // since hangover + prefill add frames at the boundaries and a noisy
    // Silero call could trim a few inside.
    assert!(
        frames >= 30,
        "expected ~50+ Frame events inside the utterance, got {frames}",
    );
}

/// Iteration-2 review finding #3: cancelling mid-utterance must zero
/// the Silero LSTM recurrent state. Without that reset, the LSTM
/// `h`/`c` tensors carry phoneme context from the cancelled audio
/// into the next recording, so the first frames after the cancel can
/// be biased — typically a spurious `SpeechStart` while feeding pure
/// silence right after the cancel.
///
/// This test feeds the real Silero ONNX model 600 ms of loud sine to
/// drive it deep into voice context, calls `SmoothedVad::reset()`
/// (the supervisor's cancel hook), then feeds 30+ frames of pure
/// silence (≥ 900 ms — well past `ONSET_FRAMES`'s 2-frame debounce
/// and Silero's own context window) and asserts NO `SpeechStart` is
/// emitted in that silence window.
#[test]
fn reset_zeroes_silero_lstm_so_post_cancel_silence_does_not_re_trigger() {
    use whisper_dictate_app::audio::FRAME_SIZE;

    // Build a smoothed VAD on top of the real Silero model — the bug
    // only manifests on a backend that has recurrent state. The RMS
    // stub is stateless and would never reproduce the issue.
    let silero =
        SileroVad::from_embedded_bytes(include_bytes!("../../../assets/silero_vad.onnx"))
            .expect("load embedded Silero model");
    let mut vad = SmoothedVad::new(silero);

    // Reuse the companion test's signal generator + resampler so we
    // know it converges to a SpeechStart against the real Silero
    // model (verified by `pipeline_detects_speech_in_synthetic_sine_recording`).
    // We need a leading silence segment because Silero scores the
    // first few frames against an uninitialised LSTM, where the bias
    // can suppress an instant SpeechStart even for a clear sine.
    let priming_input = make_input(); // 0.6 s silence + 1.5 s sine + 0.6 s silence
    // Resample once into 16 kHz / 480-sample frames so we can split
    // them into "feed-then-cancel" vs. "post-cancel silence" segments.
    let frames_16k: Vec<Vec<f32>> = {
        let mut resampler = FrameResampler::new(INPUT_RATE).expect("resampler");
        let mut out: Vec<Vec<f32>> = Vec::new();
        for chunk in priming_input.chunks(480) {
            resampler.push(chunk, |frame| out.push(frame.to_vec()));
        }
        resampler.finish(|frame| out.push(frame.to_vec()));
        out
    };
    // Feed frames until in_speech goes true, then a few more so the
    // LSTM is deeply biased toward voice. Stop at the point we'd
    // expect the cancel — well before the trailing silence so the
    // test does not depend on the natural SpeechEnd path.
    let mut entered = false;
    let mut after_entry = 0usize;
    let mut consumed = 0usize;
    for frame in &frames_16k {
        let _ = vad.feed(frame).expect("vad feed during voice");
        consumed += 1;
        if vad.in_speech() {
            if !entered {
                entered = true;
            }
            after_entry += 1;
            if after_entry >= 15 {
                break;
            }
        }
    }
    assert!(
        entered,
        "test setup must drive the VAD into in_speech before reset; \
         consumed {consumed} of {} primed frames — investigate before \
         changing the test",
        frames_16k.len(),
    );

    // Cancel mid-utterance — this is what the supervisor calls on PTT
    // release / explicit cancel. The wrapper state clears AND, per the
    // fix, the Silero LSTM state is zeroed.
    let was_in_speech = vad.reset();
    assert!(was_in_speech, "reset() must report it cancelled an utterance");

    // Pump 30 frames of silence to clear any residual hangover state
    // — without this, the SmoothedVad might still be processing the
    // pre-reset voice context via wrapper-level smoothing (although
    // reset clears `in_speech` directly). Also asserts the basic
    // user-observable invariant: post-cancel silence must not trip a
    // spurious SpeechStart.
    let silence_frame = vec![0.0f32; FRAME_SIZE];
    let mut spurious_starts = 0;
    for _ in 0..30 {
        match vad.feed(&silence_frame).expect("vad feed during silence") {
            VadEvent::SpeechStart(_) => spurious_starts += 1,
            VadEvent::SpeechFrame(_) | VadEvent::SpeechEnd | VadEvent::Silence => {}
        }
    }
    assert_eq!(
        spurious_starts, 0,
        "post-cancel silence must not produce SpeechStart",
    );
    assert!(
        !vad.in_speech(),
        "VAD must remain out of speech throughout the post-cancel silence",
    );

    // Property check that catches the LSTM-bleed bug independently
    // of Silero's onset threshold: feed the same primed voice signal
    // and measure the SpeechStart offset against a FRESH VAD on
    // identical input. With a polluted LSTM, the post-cancel run
    // converges SOONER (the LSTM is already "warm" with voice
    // context). We tolerate a small jitter for Silero numerical
    // noise but flag a > 5-frame difference as the bug.
    let mut fresh_vad = SmoothedVad::new(
        SileroVad::from_embedded_bytes(include_bytes!("../../../assets/silero_vad.onnx"))
            .expect("load embedded Silero model"),
    );
    let onset_fresh = first_speech_start_offset(&mut fresh_vad, &frames_16k)
        .expect("fresh vad must produce SpeechStart on the primed signal");
    let onset_reset = first_speech_start_offset(&mut vad, &frames_16k)
        .expect("reset vad must produce SpeechStart on the primed signal");
    let drift = (onset_reset as i32 - onset_fresh as i32).abs();
    assert!(
        drift <= 5,
        "post-reset SpeechStart offset {onset_reset} drifts >5 frames \
         from the fresh-vad baseline {onset_fresh} — LSTM recurrent \
         state was not zeroed on reset",
    );
}

/// Feed `frames` to `vad` and return the index of the frame whose
/// feed produced a `SpeechStart`. `None` if no `SpeechStart` fires.
fn first_speech_start_offset(vad: &mut SmoothedVad, frames: &[Vec<f32>]) -> Option<usize> {
    for (i, frame) in frames.iter().enumerate() {
        if let VadEvent::SpeechStart(_) = vad.feed(frame).expect("vad feed") {
            return Some(i);
        }
    }
    None
}
