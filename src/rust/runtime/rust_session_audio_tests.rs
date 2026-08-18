//! Tests for [`super::pump_loop_with_recv`] -- the pure-logic core of
//! the rust-session audio pump. Drives the loop with synthetic
//! [`PipelineEvent`]s so we cover the four behaviours
//! ([`PipelineEvent::Frame`] forwarding, [`PipelineEvent::DeviceError`]
//! termination, channel-close exit)
//! without spinning up cpal capture.

use std::sync::{Arc, Mutex};

use super::{
    pump_loop_with_recv, reset_recovery_attempt_after_frame, schedule_device_recovery,
    DEVICE_RECOVERY_ATTEMPTS,
};
use crate::audio::PipelineEvent;

/// Drive the loop against an in-memory event queue. Returns the
/// captured per-call sinks for assertion.
fn drive(events: Vec<PipelineEvent>) -> (Vec<Vec<f32>>, Vec<String>, Option<String>) {
    let frames = Arc::new(Mutex::new(Vec::<Vec<f32>>::new()));
    let logs = Arc::new(Mutex::new(Vec::<String>::new()));
    let queue = Arc::new(Mutex::new(events.into_iter()));
    let frames_for_sink = Arc::clone(&frames);
    let logs_for_sink = Arc::clone(&logs);
    let device_error = pump_loop_with_recv(
        || queue.lock().unwrap().next(),
        move |frame| {
            frames_for_sink.lock().unwrap().push(frame.to_vec());
            true
        },
        |_| {},
        move |line| logs_for_sink.lock().unwrap().push(line),
    );
    let frames = Arc::try_unwrap(frames).unwrap().into_inner().unwrap();
    let logs = Arc::try_unwrap(logs).unwrap().into_inner().unwrap();
    (frames, logs, device_error)
}

#[test]
fn forwards_each_frame_to_push_frame_sink() {
    let (frames, logs, device_error) = drive(vec![
        PipelineEvent::Frame(vec![0.1, 0.2, 0.3]),
        PipelineEvent::Frame(vec![0.4, 0.5]),
    ]);
    assert_eq!(frames, vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5]]);
    assert!(logs.is_empty(), "no logs expected on the happy path");
    assert!(device_error.is_none());
}

#[test]
fn device_error_stops_the_current_pump_and_is_returned_to_the_recovery_owner() {
    // Per the wire contract documented on
    // `PipelineEvent::DeviceError`, the pump MUST stop after a
    // device error -- subsequent events must NOT be processed even
    // when they are still in the queue.
    let (frames, logs, device_error) = drive(vec![
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
    assert_eq!(device_error.as_deref(), Some("xrun in callback"));
}

#[test]
fn drains_and_discards_frames_while_transcription_owns_the_session() {
    let events = Arc::new(Mutex::new(
        vec![
            PipelineEvent::Frame(vec![1.0]),
            PipelineEvent::Frame(vec![2.0]),
            PipelineEvent::Frame(vec![3.0]),
        ]
        .into_iter(),
    ));
    let accepted = Arc::new(Mutex::new(Vec::<Vec<f32>>::new()));
    let reports = Arc::new(Mutex::new(Vec::<usize>::new()));
    let attempts = Arc::new(Mutex::new(0usize));
    let accepted_sink = Arc::clone(&accepted);
    let reports_sink = Arc::clone(&reports);
    let attempts_sink = Arc::clone(&attempts);

    let device_error = pump_loop_with_recv(
        || events.lock().unwrap().next(),
        move |frame| {
            let mut attempt = attempts_sink.lock().unwrap();
            *attempt += 1;
            if *attempt <= 2 {
                false
            } else {
                accepted_sink.lock().unwrap().push(frame.to_vec());
                true
            }
        },
        move |count| reports_sink.lock().unwrap().push(count),
        |_| {},
    );

    assert!(device_error.is_none());
    assert_eq!(*accepted.lock().unwrap(), vec![vec![3.0]]);
    assert_eq!(
        *reports.lock().unwrap(),
        vec![2],
        "the busy interval must be summarized once after draining"
    );
}

#[test]
fn production_device_error_emits_a_terminal_supervisor_exit() {
    let source = include_str!("rust_session_audio.rs");
    assert!(
        source.contains("RuntimeEvent::Exited { code: Some(1) }"),
        "terminal CPAL failure must notify RuntimeSupervisor"
    );
}

#[test]
fn channel_close_exits_loop() {
    // recv_next returning None (the production case when the cpal
    // stream is dropped via `AudioPump::drop`) must end the loop
    // immediately without panicking.
    let (frames, logs, device_error) = drive(vec![]);
    assert!(frames.is_empty());
    assert!(logs.is_empty());
    assert!(device_error.is_none());
}

#[test]
fn stopped_runtime_does_not_schedule_a_device_recovery() {
    let stop_requested = std::sync::atomic::AtomicBool::new(true);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut attempt = 0;

    assert!(!schedule_device_recovery(
        &stop_requested,
        &tx,
        None,
        &mut attempt,
        "device invalidated".to_owned(),
    ));
    assert_eq!(attempt, 0);
    assert!(
        rx.try_recv().is_err(),
        "stop should not emit a recovery log"
    );
}

#[test]
fn device_error_schedules_one_bounded_reopen_attempt() {
    let stop_requested = std::sync::atomic::AtomicBool::new(false);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut attempt = 0;

    assert!(schedule_device_recovery(
        &stop_requested,
        &tx,
        None,
        &mut attempt,
        "device invalidated".to_owned(),
    ));
    assert_eq!(attempt, 1);
    assert!(matches!(
        rx.recv().unwrap(),
        crate::runtime::RuntimeEvent::Stderr(message)
            if message.contains("attempt 1/") && message.contains("device invalidated")
    ));
}

#[test]
fn exhausted_recovery_emits_terminal_exit_without_another_reopen() {
    let stop_requested = std::sync::atomic::AtomicBool::new(false);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut attempt = DEVICE_RECOVERY_ATTEMPTS;

    assert!(!schedule_device_recovery(
        &stop_requested,
        &tx,
        None,
        &mut attempt,
        "device remained unavailable".to_owned(),
    ));
    assert_eq!(attempt, DEVICE_RECOVERY_ATTEMPTS);
    assert!(matches!(
        rx.recv().unwrap(),
        crate::runtime::RuntimeEvent::Stderr(message)
            if message.contains("recovery exhausted") && message.contains("device remained unavailable")
    ));
    assert!(matches!(
        rx.recv().unwrap(),
        crate::runtime::RuntimeEvent::Exited { code: Some(1) }
    ));
}

#[test]
fn healthy_replacement_frame_resets_the_per_incident_retry_budget() {
    let mut attempt = DEVICE_RECOVERY_ATTEMPTS;
    reset_recovery_attempt_after_frame(&mut attempt, true);
    assert_eq!(attempt, 0);

    reset_recovery_attempt_after_frame(&mut attempt, false);
    assert_eq!(attempt, 0);
}
