//! Tests for the non-blocking frame forwarder.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{forward_frames, ForwardEnd, FrameSink, SessionFrameSink, PENDING_FRAME_LIMIT};
use crate::audio::PipelineEvent;

#[derive(Default)]
struct ScriptedSink {
    accepted: Mutex<Vec<Vec<f32>>>,
    busy_attempts_left: AtomicUsize,
    always_busy: AtomicBool,
}

impl FrameSink for ScriptedSink {
    fn try_push(&self, frame: &[f32]) -> bool {
        if self.always_busy.load(Ordering::SeqCst) {
            return false;
        }
        let busy = self
            .busy_attempts_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok();
        if busy {
            return false;
        }
        self.accepted.lock().unwrap().push(frame.to_vec());
        true
    }
}

fn run(events: Vec<PipelineEvent>, sink: &ScriptedSink) -> ForwardEnd {
    let mut events = events.into_iter();
    forward_frames(|| events.next(), sink)
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
    let end = run(
        vec![
            PipelineEvent::Frame(vec![1.0]),
            PipelineEvent::Frame(vec![2.0]),
            PipelineEvent::Frame(vec![3.0]),
        ],
        &sink,
    );
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
fn session_sink_never_waits_for_a_locked_session() {
    let session = super::super::rust_session_sink::make_session();
    let sink = SessionFrameSink::new(Arc::clone(&session));
    let guard = session.lock().unwrap();
    assert!(!sink.try_push(&[0.5]), "a held session lock reports busy");
    drop(guard);
    assert!(sink.try_push(&[0.5]));
}
