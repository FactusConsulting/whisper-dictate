//! Tests for [`super::pump_loop_with_recv`] -- the pure-logic core of
//! the rust-session audio pump. Drives the loop with synthetic
//! [`PipelineEvent`]s so we cover the four behaviours
//! ([`PipelineEvent::Frame`] forwarding, idle-marker drop,
//! [`PipelineEvent::DeviceError`] termination, channel-close exit)
//! without spinning up cpal / Silero.

use std::sync::{Arc, Mutex};

use super::pump_loop_with_recv;
use crate::audio::PipelineEvent;

/// Drive the loop against an in-memory event queue. Returns the
/// captured per-call sinks for assertion.
fn drive(events: Vec<PipelineEvent>) -> (Vec<Vec<f32>>, Vec<String>) {
    let frames = Arc::new(Mutex::new(Vec::<Vec<f32>>::new()));
    let logs = Arc::new(Mutex::new(Vec::<String>::new()));
    let queue = Arc::new(Mutex::new(events.into_iter()));
    let frames_for_sink = Arc::clone(&frames);
    let logs_for_sink = Arc::clone(&logs);
    pump_loop_with_recv(
        || queue.lock().unwrap().next(),
        move |frame| frames_for_sink.lock().unwrap().push(frame.to_vec()),
        move |line| logs_for_sink.lock().unwrap().push(line),
    );
    let frames = Arc::try_unwrap(frames).unwrap().into_inner().unwrap();
    let logs = Arc::try_unwrap(logs).unwrap().into_inner().unwrap();
    (frames, logs)
}

#[test]
fn forwards_each_frame_to_push_frame_sink() {
    let (frames, logs) = drive(vec![
        PipelineEvent::Frame(vec![0.1, 0.2, 0.3]),
        PipelineEvent::Frame(vec![0.4, 0.5]),
    ]);
    assert_eq!(frames, vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5]]);
    assert!(logs.is_empty(), "no logs expected on the happy path");
}

#[test]
fn drops_speech_markers_and_cancelled_silently() {
    // The pump must not forward SpeechStart/End/Cancelled as frames,
    // and must not log them either -- the PTT coordinator owns
    // recording lifecycle and these markers carry no payload the
    // session can consume.
    let (frames, logs) = drive(vec![
        PipelineEvent::SpeechStart,
        PipelineEvent::Frame(vec![1.0]),
        PipelineEvent::SpeechEnd,
        PipelineEvent::Cancelled,
        PipelineEvent::Frame(vec![2.0]),
    ]);
    assert_eq!(
        frames,
        vec![vec![1.0], vec![2.0]],
        "speech markers must not appear as frames"
    );
    assert!(logs.is_empty(), "speech markers must not produce log lines");
}

#[test]
fn device_error_terminates_pump_after_emitting_log_line() {
    // Per the wire contract documented on
    // `PipelineEvent::DeviceError`, the pump MUST stop after a
    // device error -- subsequent events must NOT be processed even
    // when they are still in the queue.
    let (frames, logs) = drive(vec![
        PipelineEvent::Frame(vec![1.0]),
        PipelineEvent::DeviceError("xrun in callback".to_owned()),
        // These events follow the DeviceError -- the pump must NOT
        // see them; if it does this assertion will trip.
        PipelineEvent::Frame(vec![2.0]),
        PipelineEvent::Frame(vec![3.0]),
    ]);
    assert_eq!(frames, vec![vec![1.0]], "no frames after the DeviceError");
    assert_eq!(logs.len(), 1, "exactly one log line per DeviceError");
    assert!(
        logs[0].starts_with("[rust-session-audio] device error:"),
        "log line must be prefixed and tagged, got: {}",
        logs[0]
    );
    assert!(
        logs[0].contains("xrun in callback"),
        "log line must carry the original message, got: {}",
        logs[0]
    );
}

#[test]
fn channel_close_exits_loop() {
    // recv_next returning None (the production case when the cpal
    // stream is dropped via `AudioPump::drop`) must end the loop
    // immediately without panicking.
    let (frames, logs) = drive(vec![]);
    assert!(frames.is_empty());
    assert!(logs.is_empty());
}
