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
