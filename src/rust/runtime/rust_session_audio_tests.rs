//! Tests for [`super::pump_loop_with_recv`] -- the pure-logic core of
//! the rust-session audio pump. Drives the loop with synthetic
//! [`PipelineEvent`]s so we cover the four behaviours
//! ([`PipelineEvent::Frame`] forwarding, [`PipelineEvent::DeviceError`]
//! termination, channel-close exit)
//! without spinning up cpal capture.

use std::cell::Cell;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use super::{
    apply_validated_recovery, mark_capture_unavailable, next_recovery_target, open_recovery_target,
    publish_recovery_status, pump_loop_with_recv, record_recovery_open_timeout,
    recv_pipeline_event, report_recovery_open_failure, reset_recovery_attempt_after_frame,
    schedule_device_recovery, send_audio_status, should_try_system_default,
    start_initial_capture_with, take_validated_recovery_target, RecoveryTarget,
    DEVICE_RECOVERY_ATTEMPTS, RECOVERY_HEALTHY_FRAME_COUNT,
};
use crate::audio::PipelineEvent;

/// Drive the loop against an in-memory event queue. Returns the
/// captured per-call sinks for assertion.
fn drive(events: Vec<PipelineEvent>) -> (Vec<Vec<f32>>, Option<String>) {
    let frames = Arc::new(Mutex::new(Vec::<Vec<f32>>::new()));
    let queue = Arc::new(Mutex::new(events.into_iter()));
    let frames_for_sink = Arc::clone(&frames);
    let device_error = pump_loop_with_recv(
        || queue.lock().unwrap().next(),
        move |frame| {
            frames_for_sink.lock().unwrap().push(frame.to_vec());
            true
        },
        |_| {},
    );
    let frames = Arc::try_unwrap(frames).unwrap().into_inner().unwrap();
    (frames, device_error)
}

#[test]
fn forwards_each_frame_to_push_frame_sink() {
    let (frames, device_error) = drive(vec![
        PipelineEvent::Frame(vec![0.1, 0.2, 0.3]),
        PipelineEvent::Frame(vec![0.4, 0.5]),
    ]);
    assert_eq!(frames, vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5]]);
    assert!(device_error.is_none());
}

#[test]
fn device_error_stops_the_current_pump_and_is_returned_to_the_recovery_owner() {
    // Per the wire contract documented on
    // `PipelineEvent::DeviceError`, the pump MUST stop after a
    // device error -- subsequent events must NOT be processed even
    // when they are still in the queue.
    let (frames, device_error) = drive(vec![
        PipelineEvent::Frame(vec![1.0]),
        PipelineEvent::DeviceError("xrun in callback".to_owned()),
        // These events follow the DeviceError -- the pump must NOT
        // see them; if it does this assertion will trip.
        PipelineEvent::Frame(vec![2.0]),
        PipelineEvent::Frame(vec![3.0]),
    ]);
    assert_eq!(frames, vec![vec![1.0]], "no frames after the DeviceError");
    // The recovery owner emits the sole diagnostic, so this pure loop must
    // return details rather than producing a second log line itself.
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
fn production_device_error_uses_default_input_without_terminating_supervisor() {
    let source = include_str!("rust_session_audio.rs");
    assert!(
        source.contains("RecoveryTarget::SystemDefault"),
        "device recovery must select the operating system default input"
    );
    assert!(
        !source.contains("RuntimeEvent::Exited { code: Some(1) }"),
        "a device loss must not terminate the runtime supervisor"
    );
}

#[test]
fn channel_close_exits_loop() {
    // recv_next returning None (the production case when the cpal
    // stream is dropped via `AudioPump::drop`) must end the loop
    // immediately without panicking.
    let (frames, device_error) = drive(vec![]);
    assert!(frames.is_empty());
    assert!(device_error.is_none());
}

#[test]
fn stopped_runtime_does_not_schedule_a_device_recovery() {
    let stop_requested = std::sync::atomic::AtomicBool::new(true);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut attempt = 0;

    assert!(schedule_device_recovery(
        &stop_requested,
        &tx,
        None,
        &mut attempt,
        "USB microphone",
        "device invalidated".to_owned(),
    )
    .is_none());
    assert_eq!(attempt, 0);
    assert!(
        rx.try_recv().is_err(),
        "stop should not emit a recovery log"
    );
}

#[test]
fn failed_open_after_stop_does_not_publish_a_retrying_status() {
    let stop_requested = std::sync::atomic::AtomicBool::new(true);
    let (tx, rx) = std::sync::mpsc::channel();

    assert!(!report_recovery_open_failure(
        &stop_requested,
        &tx,
        None,
        RecoveryTarget::SystemDefault,
        "device disappeared during teardown",
        true,
    ));
    assert!(rx.try_recv().is_err());
}

#[test]
fn configured_start_timeout_permanently_skips_that_selector() {
    let mut attempt = 1;
    let mut circuit_open = false;
    assert!(record_recovery_open_timeout(
        RecoveryTarget::Configured,
        true,
        &mut attempt,
        &mut circuit_open,
    ));
    assert!(circuit_open);
    assert_eq!(attempt, DEVICE_RECOVERY_ATTEMPTS);
    assert_eq!(
        next_recovery_target("USB microphone", &mut attempt),
        RecoveryTarget::SystemDefault
    );

    reset_recovery_attempt_after_frame(&mut attempt, RECOVERY_HEALTHY_FRAME_COUNT, circuit_open);
    assert_eq!(attempt, DEVICE_RECOVERY_ATTEMPTS);
}

#[test]
fn system_default_start_timeout_pauses_recovery() {
    let mut attempt = DEVICE_RECOVERY_ATTEMPTS;
    let mut circuit_open = true;
    assert!(!record_recovery_open_timeout(
        RecoveryTarget::SystemDefault,
        true,
        &mut attempt,
        &mut circuit_open,
    ));
}

#[test]
fn timed_out_default_reports_paused_recovery_instead_of_retrying() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let stop_requested = std::sync::atomic::AtomicBool::new(false);
    let (tx, rx) = std::sync::mpsc::channel();
    let wake_count = Arc::new(AtomicUsize::new(0));
    let wake_sink = Arc::clone(&wake_count);
    let notifier: crate::runtime::RepaintNotifier = Arc::new(move || {
        wake_sink.fetch_add(1, Ordering::Relaxed);
    });

    assert!(report_recovery_open_failure(
        &stop_requested,
        &tx,
        Some(&notifier),
        RecoveryTarget::SystemDefault,
        "timed out waiting for the input stream",
        false,
    ));
    let crate::runtime::RuntimeEvent::Worker(event) = rx.recv().unwrap() else {
        panic!("expected persistent worker error");
    };
    assert_eq!(event.payload["reason"], "device_unusable");
    let error = event.payload["error"].as_str().unwrap();
    assert!(error.contains("recovery is paused"), "{error}");
    assert!(!error.contains("retrying in the background"), "{error}");
    assert_eq!(wake_count.load(Ordering::Relaxed), 1);
}

#[test]
fn device_error_schedules_one_bounded_reopen_attempt() {
    let stop_requested = std::sync::atomic::AtomicBool::new(false);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut attempt = 0;

    assert_eq!(
        schedule_device_recovery(
            &stop_requested,
            &tx,
            None,
            &mut attempt,
            "USB microphone",
            "device invalidated".to_owned(),
        ),
        Some(RecoveryTarget::Configured)
    );
    assert_eq!(attempt, 1);
    assert!(matches!(
        rx.recv().unwrap(),
        crate::runtime::RuntimeEvent::Stderr(message)
            if message.contains("attempt 1/") && message.contains("device invalidated")
    ));
}

#[test]
fn exhausted_configured_recovery_falls_back_to_system_default_without_exiting() {
    let stop_requested = std::sync::atomic::AtomicBool::new(false);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut attempt = DEVICE_RECOVERY_ATTEMPTS;

    assert_eq!(
        schedule_device_recovery(
            &stop_requested,
            &tx,
            None,
            &mut attempt,
            "USB microphone",
            "device remained unavailable".to_owned(),
        ),
        Some(RecoveryTarget::SystemDefault)
    );
    assert_eq!(attempt, DEVICE_RECOVERY_ATTEMPTS);
    assert!(matches!(
        rx.recv().unwrap(),
        crate::runtime::RuntimeEvent::Stderr(message)
            if message.contains("system default input")
                && message.contains("runtime stays active")
                && message.contains("device remained unavailable")
    ));
    assert!(
        rx.try_recv().is_err(),
        "fallback must not terminate the runtime"
    );
}

#[test]
fn recovery_tries_configured_input_three_times_then_uses_system_default() {
    let mut attempt = 0;
    for expected_attempt in 1..=DEVICE_RECOVERY_ATTEMPTS {
        assert_eq!(
            next_recovery_target("USB microphone", &mut attempt),
            RecoveryTarget::Configured
        );
        assert_eq!(attempt, expected_attempt);
    }
    assert_eq!(
        next_recovery_target("USB microphone", &mut attempt),
        RecoveryTarget::SystemDefault
    );
    assert_eq!(attempt, DEVICE_RECOVERY_ATTEMPTS);
}

#[test]
fn system_default_selector_reopens_the_system_default_without_a_retry_budget() {
    let mut attempt = 0;
    assert_eq!(
        next_recovery_target("", &mut attempt),
        RecoveryTarget::SystemDefault
    );
    assert_eq!(attempt, 0);
}

#[test]
fn startup_only_falls_back_when_a_named_device_was_configured() {
    assert!(should_try_system_default("USB microphone"));
    assert!(!should_try_system_default(" \t\n"));
}

#[test]
fn startup_fallback_preserves_the_configured_device_error() {
    let mut selectors = Vec::new();
    let (capture, configured_error) = start_initial_capture_with("USB microphone", |selector| {
        selectors.push(selector.to_owned());
        if selector.is_empty() {
            Ok("default capture")
        } else {
            Err(anyhow::anyhow!("named device disappeared"))
        }
    })
    .unwrap();
    assert_eq!(capture, "default capture");
    assert_eq!(
        configured_error.as_deref(),
        Some("named device disappeared")
    );
    assert_eq!(selectors, ["USB microphone", ""]);
}

#[test]
fn startup_fallback_reports_both_open_failures() {
    let error = start_initial_capture_with("USB microphone", |selector| {
        Err::<(), _>(anyhow::anyhow!("cannot open {selector:?}"))
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("configured input (cannot open \"USB microphone\")"));
    assert!(error.contains("system default input also failed (cannot open \"\")"));
}

#[test]
fn system_default_recovery_opens_the_empty_cpal_selector() {
    let opened = Arc::new(Mutex::new(String::new()));
    let opened_sink = Arc::clone(&opened);
    let result = open_recovery_target(
        RecoveryTarget::SystemDefault,
        "Disconnected USB microphone",
        move |selector| {
            *opened_sink.lock().unwrap() = selector.to_owned();
            Ok::<_, ()>("replacement pipeline")
        },
    );
    assert_eq!(result, Ok("replacement pipeline"));
    assert_eq!(&*opened.lock().unwrap(), "");
}

#[test]
fn fallback_status_reports_effective_device_without_changing_saved_selector() {
    let (tx, rx) = std::sync::mpsc::channel();
    send_audio_status(&tx, "audio-fallback", Some("System default"), None, None);
    let crate::runtime::RuntimeEvent::Worker(event) = rx.recv().unwrap() else {
        panic!("expected worker status");
    };
    assert_eq!(event.state.as_deref(), Some("audio-fallback"));
    assert_eq!(event.payload["audio_device"], "System default");
}

#[test]
fn every_recovery_reports_the_effective_device_and_wakes_the_ui() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    for (target, expected_device) in [
        (RecoveryTarget::Configured, "USB microphone"),
        (RecoveryTarget::SystemDefault, "System default"),
    ] {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake_count_sink = Arc::clone(&wake_count);
        let notifier: crate::runtime::RepaintNotifier = Arc::new(move || {
            wake_count_sink.fetch_add(1, Ordering::Relaxed);
        });

        publish_recovery_status(&tx, Some(&notifier), target, "USB microphone");

        let crate::runtime::RuntimeEvent::Worker(event) = rx.recv().unwrap() else {
            panic!("expected worker status");
        };
        assert_eq!(event.state.as_deref(), Some("audio-recovered"));
        assert_eq!(event.payload["audio_device"], expected_device);
        assert_eq!(wake_count.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn unavailable_default_status_is_a_persistent_device_error() {
    let (tx, rx) = std::sync::mpsc::channel();
    send_audio_status(
        &tx,
        "error",
        None,
        Some("device_unusable"),
        Some("System default microphone is unavailable"),
    );
    let crate::runtime::RuntimeEvent::Worker(event) = rx.recv().unwrap() else {
        panic!("expected worker status");
    };
    assert_eq!(event.payload["reason"], "device_unusable");
    assert_eq!(
        event.payload["error"],
        "System default microphone is unavailable"
    );
}

#[test]
fn only_a_sustained_replacement_stream_resets_the_retry_budget() {
    let mut attempt = DEVICE_RECOVERY_ATTEMPTS;
    reset_recovery_attempt_after_frame(&mut attempt, 1, false);
    assert_eq!(attempt, DEVICE_RECOVERY_ATTEMPTS);

    reset_recovery_attempt_after_frame(&mut attempt, RECOVERY_HEALTHY_FRAME_COUNT, false);
    assert_eq!(attempt, 0);

    reset_recovery_attempt_after_frame(&mut attempt, 0, false);
    assert_eq!(attempt, 0);
}

#[test]
fn recovery_is_reported_once_only_after_the_candidate_is_healthy() {
    let candidate = Cell::new(Some(RecoveryTarget::SystemDefault));

    assert_eq!(
        take_validated_recovery_target(&candidate, RECOVERY_HEALTHY_FRAME_COUNT - 1),
        None
    );
    assert_eq!(candidate.get(), Some(RecoveryTarget::SystemDefault));
    assert_eq!(
        take_validated_recovery_target(&candidate, RECOVERY_HEALTHY_FRAME_COUNT),
        Some(RecoveryTarget::SystemDefault)
    );
    assert_eq!(
        take_validated_recovery_target(&candidate, RECOVERY_HEALTHY_FRAME_COUNT + 1),
        None,
        "a validated stream must publish recovery only once"
    );
}

#[test]
fn validated_recovery_updates_effective_device_without_the_session_lock() {
    let effective_device = RwLock::new("Disconnected USB microphone".to_owned());
    let candidate = Cell::new(Some(RecoveryTarget::SystemDefault));
    let stop_requested = std::sync::atomic::AtomicBool::new(false);

    assert_eq!(
        apply_validated_recovery(
            &stop_requested,
            &effective_device,
            &candidate,
            RECOVERY_HEALTHY_FRAME_COUNT,
            "Disconnected USB microphone",
        ),
        Some(RecoveryTarget::SystemDefault)
    );
    assert_eq!(
        &*effective_device
            .read()
            .unwrap_or_else(|poison| poison.into_inner()),
        "System default"
    );
}

#[test]
fn validation_timeout_turns_a_silent_candidate_into_a_device_error() {
    let (_tx, rx) = crate::audio::raw::pipeline_event_channel(1);
    let event = recv_pipeline_event(&rx, Some(std::time::Instant::now()), Duration::ZERO)
        .expect("timeout event");
    assert!(matches!(
        event,
        PipelineEvent::DeviceError(message)
            if message.contains("recovery candidate") && message.contains("healthy frames")
    ));
}

#[test]
fn validation_deadline_is_not_reset_by_queued_candidate_frames() {
    let (tx, rx) = crate::audio::raw::pipeline_event_channel(1);
    tx.try_send_latest(PipelineEvent::Frame(vec![1.0]))
        .expect("queue candidate frame");
    let expired_deadline = std::time::Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("representable deadline");

    let event = recv_pipeline_event(&rx, Some(expired_deadline), Duration::from_secs(5))
        .expect("timeout event");

    assert!(matches!(event, PipelineEvent::DeviceError(_)));
}

#[test]
fn teardown_suppresses_validated_recovery_success() {
    let effective_device = RwLock::new("Disconnected USB microphone".to_owned());
    let candidate = Cell::new(Some(RecoveryTarget::SystemDefault));
    let stop_requested = std::sync::atomic::AtomicBool::new(true);

    assert_eq!(
        apply_validated_recovery(
            &stop_requested,
            &effective_device,
            &candidate,
            RECOVERY_HEALTHY_FRAME_COUNT,
            "Disconnected USB microphone",
        ),
        None
    );
    assert_eq!(
        &*effective_device.read().unwrap(),
        "Disconnected USB microphone"
    );
}

#[test]
fn paused_capture_suppresses_the_effective_device_from_later_statuses() {
    let effective_device = RwLock::new("System default".to_owned());
    mark_capture_unavailable(&effective_device);
    assert!(effective_device.read().unwrap().is_empty());
}
