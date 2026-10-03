//! Tests for the non-blocking frame forwarder.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{
    forward_frames, ForwardEnd, FrameSink, PushOutcome, SessionFrameSink, PENDING_FRAME_LIMIT,
};
use crate::audio::PipelineEvent;
use crate::dictate::{DictateSession, SessionConfig};

#[derive(Default)]
struct ScriptedSink {
    accepted: Mutex<Vec<Vec<f32>>>,
    busy_attempts_left: AtomicUsize,
    always_busy: AtomicBool,
    capacity: Option<usize>,
}

impl FrameSink for ScriptedSink {
    fn try_push_with_cap(&self, frame: &[f32], on_full: &mut dyn FnMut()) -> PushOutcome {
        if self.always_busy.load(Ordering::SeqCst) {
            return PushOutcome::Busy;
        }
        let busy = self
            .busy_attempts_left
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok();
        if busy {
            return PushOutcome::Busy;
        }
        let mut accepted = self.accepted.lock().unwrap();
        accepted.push(frame.to_vec());
        if self.capacity.is_some_and(|cap| accepted.len() >= cap) {
            on_full();
            PushOutcome::RecordingFull
        } else {
            PushOutcome::Accepted
        }
    }
}

fn run(events: Vec<PipelineEvent>, sink: &ScriptedSink) -> ForwardEnd {
    let mut events = events.into_iter();
    forward_frames(|| events.next(), sink)
}

fn frames(values: &[f32]) -> Vec<PipelineEvent> {
    values
        .iter()
        .map(|value| PipelineEvent::Frame(vec![*value]))
        .collect()
}

#[test]
fn forwards_frames_in_order_until_the_stream_closes() {
    let sink = ScriptedSink::default();
    let end = run(
        vec![
            PipelineEvent::Frame(vec![0.1, 0.2]),
            PipelineEvent::Frame(vec![0.3]),
        ],
        &sink,
    );
    assert_eq!(end, ForwardEnd::Closed);
    assert_eq!(
        *sink.accepted.lock().unwrap(),
        vec![vec![0.1, 0.2], vec![0.3]]
    );
}

#[test]
fn final_report_counts_evictions_and_the_busy_session_tail() {
    let sink = ScriptedSink {
        always_busy: AtomicBool::new(true),
        ..Default::default()
    };
    let count = PENDING_FRAME_LIMIT + 17;
    let mut events = (0..count).map(|index| PipelineEvent::Frame(vec![index as f32]));
    let report = super::forward_frames_with_report(|| events.next(), &sink);
    assert_eq!(report.end, ForwardEnd::Closed);
    assert_eq!(report.pending_frames_dropped, count as u64);
    assert!(sink.accepted.lock().unwrap().is_empty());
}

#[test]
fn device_error_stops_forwarding_and_ignores_later_events() {
    let sink = ScriptedSink::default();
    let end = run(
        vec![
            PipelineEvent::Frame(vec![1.0]),
            PipelineEvent::DeviceError("unplugged".to_owned()),
            PipelineEvent::Frame(vec![2.0]),
        ],
        &sink,
    );
    assert_eq!(end, ForwardEnd::DeviceError("unplugged".to_owned()));
    assert_eq!(*sink.accepted.lock().unwrap(), vec![vec![1.0]]);
}

#[test]
fn frames_meeting_a_busy_session_are_kept_and_delivered_in_order() {
    let sink = ScriptedSink {
        busy_attempts_left: AtomicUsize::new(2),
        ..Default::default()
    };
    let end = run(frames(&[1.0, 2.0, 3.0]), &sink);
    assert_eq!(end, ForwardEnd::Closed);
    assert_eq!(
        *sink.accepted.lock().unwrap(),
        vec![vec![1.0], vec![2.0], vec![3.0]],
        "the first word must survive a session that was briefly locked"
    );
}

#[test]
fn pending_buffer_is_bounded_and_keeps_the_newest_audio() {
    let sink = Arc::new(ScriptedSink::default());
    sink.always_busy.store(true, Ordering::SeqCst);
    let total = PENDING_FRAME_LIMIT + 5;
    let mut sent = 0usize;
    let sink_for_recv = Arc::clone(&sink);
    let end = forward_frames(
        || {
            if sent == total {
                sink_for_recv.always_busy.store(false, Ordering::SeqCst);
                return None;
            }
            sent += 1;
            Some(PipelineEvent::Frame(vec![sent as f32]))
        },
        sink.as_ref(),
    );
    assert_eq!(end, ForwardEnd::Closed);
    let accepted = sink.accepted.lock().unwrap();
    assert_eq!(accepted.len(), PENDING_FRAME_LIMIT);
    assert_eq!(accepted.first(), Some(&vec![6.0]));
    assert_eq!(accepted.last(), Some(&vec![total as f32]));
}

#[test]
fn a_full_recording_ends_forwarding_and_drops_later_audio() {
    let sink = ScriptedSink {
        capacity: Some(2),
        ..Default::default()
    };
    let end = run(frames(&[1.0, 2.0, 3.0]), &sink);
    assert_eq!(end, ForwardEnd::RecordingFull);
    assert_eq!(*sink.accepted.lock().unwrap(), vec![vec![1.0], vec![2.0]]);
}

#[test]
fn a_full_recording_is_detected_while_flushing_pending_frames() {
    let sink = ScriptedSink {
        busy_attempts_left: AtomicUsize::new(1),
        capacity: Some(1),
        ..Default::default()
    };
    let end = run(frames(&[1.0, 2.0, 3.0]), &sink);
    assert_eq!(end, ForwardEnd::RecordingFull);
    assert_eq!(*sink.accepted.lock().unwrap(), vec![vec![1.0]]);
}

#[test]
fn session_sink_never_waits_for_a_locked_session() {
    let session = super::super::rust_session_sink::make_session();
    let sink = SessionFrameSink::new(Arc::clone(&session));
    let guard = session.lock().unwrap();
    assert_eq!(sink.try_push(&[0.5]), PushOutcome::Busy);
    drop(guard);
    assert_eq!(sink.try_push(&[0.5]), PushOutcome::Accepted);
}

#[test]
fn session_sink_reports_when_the_recording_reaches_max_record_s() {
    let config = SessionConfig {
        // 0.001 s at 16 kHz = a 16-sample cap.
        max_record_seconds: Some(0.001),
        ..SessionConfig::default()
    };
    let session = Arc::new(Mutex::new(DictateSession::new(
        super::super::rust_session_sink::StubTranscribe,
        super::super::rust_session_sink::StubInject,
        config,
    )));
    session.lock().unwrap().start(&mut std::io::sink()).unwrap();
    let sink = SessionFrameSink::new(Arc::clone(&session));
    let mut notifications = 0;
    let mut on_full = || {
        notifications += 1;
        assert!(matches!(
            session.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
    };
    assert_eq!(
        sink.try_push_with_cap(&[0.1; 8], &mut on_full),
        PushOutcome::Accepted
    );
    assert_eq!(
        sink.try_push_with_cap(&[0.1; 8], &mut on_full),
        PushOutcome::RecordingFull
    );
    assert_eq!(notifications, 1);
}
