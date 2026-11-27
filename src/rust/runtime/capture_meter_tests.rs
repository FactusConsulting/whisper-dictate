//! Meter regressions driven by a synthetic capture stream, without audio hardware.

use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::super::AUDIO_METER_INTERVAL;
use super::{setup, wait_until, FakeFailure, RecordingCapture};
use crate::audio::{PipelineEvent, FRAME_SIZE};
use crate::runtime::RuntimeEvent;

#[test]
fn recording_meters_unnormalized_frames_after_fallback() {
    let (opener, frames, lifecycle, rig) = setup("USB mic");
    opener.fail("USB mic", FakeFailure::Error);
    assert!(lifecycle.open_for_recording());
    let pcm: Vec<_> = (0..FRAME_SIZE)
        .map(|i| if i % 2 == 0 { -0.02 } else { 0.0 })
        .collect();
    assert!(opener.feed(PipelineEvent::Frame(pcm.clone())));

    let mut events = Vec::new();
    wait_until("first captured frame reaches the meter", || {
        events.extend(rig.drain());
        events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Worker(event) if event.event == "audio"))
    });
    let events: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Worker(event) => Some(event),
            _ => None,
        })
        .collect();
    let [fallback, reading] = events.as_slice() else {
        panic!("expected fallback status then meter reading, got {events:?}");
    };
    assert_eq!(fallback.event, "status");
    assert_eq!(fallback.state.as_deref(), Some("audio-fallback"));
    assert_eq!(reading.event, "audio");
    assert_eq!(reading.state.as_deref(), Some("recording"));
    assert_eq!(reading.payload["event"], "audio");
    assert_eq!(reading.payload["active"], true);
    assert_eq!(reading.payload["audio_device"], "System default");
    assert!((reading.payload["peak"].as_f64().unwrap() - 0.02).abs() < 1.0e-6);
    assert!((reading.payload["raw_dbfs"].as_f64().unwrap() + 36.99).abs() < 0.01);
    assert!((reading.payload["level"].as_f64().unwrap() - 0.3835).abs() < 0.001);

    // The deadline is set before forwarding the first frame. Wait for that
    // frame's delivery attempt before starting the interval for the next one.
    wait_until("first metered frame is forwarded", || {
        frames.attempts() == 1
    });
    thread::sleep(AUDIO_METER_INTERVAL);
    assert!(opener.feed(PipelineEvent::Frame(vec![0.0; FRAME_SIZE])));
    let event = rig.rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let RuntimeEvent::Worker(silence) = event else {
        panic!("expected meter reading for silence, got {event:?}");
    };
    assert_eq!(silence.event, "audio");
    assert_eq!(silence.payload["active"], true, "silence is not a close");
    assert_eq!(silence.payload["level"], 0.0);
    assert_eq!(silence.payload["raw_dbfs"], -120.0);
    assert_eq!(silence.payload["peak"], 0.0);

    lifecycle.close_for_recording();
    assert_eq!(frames.frames(), [pcm, vec![0.0; FRAME_SIZE]]);
}

#[test]
fn closing_or_stopping_suppresses_meter_events_from_drained_frames() {
    for runtime_stop in [false, true] {
        let (opener, frames, lifecycle, rig) = setup("USB mic");
        let (run_tx, run_rx) = mpsc::channel();
        let run_rx = Arc::new(Mutex::new(run_rx));
        let lifecycle = lifecycle.with_thread_spawner(Arc::new(move |job| {
            let run_rx = Arc::clone(&run_rx);
            thread::Builder::new().spawn(move || {
                run_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                job();
            })
        }));
        assert!(lifecycle.open_for_recording());
        let pcm = vec![0.5; FRAME_SIZE];
        assert!(opener.feed(PipelineEvent::Frame(pcm.clone())));

        thread::scope(|scope| {
            let closing = scope.spawn(|| {
                if runtime_stop {
                    (lifecycle.capture_stop())();
                } else {
                    lifecycle.close_for_recording();
                }
            });
            wait_until("stream closes before the forwarder drains", || {
                opener.open_streams() == 0
            });
            run_tx.send(()).unwrap();
            closing.join().unwrap();
        });
        lifecycle.close_for_recording();
        assert_eq!(frames.frames(), [pcm], "closing still forwards queued PCM");
        assert!(rig.drain().iter().all(|event| {
            !matches!(event, RuntimeEvent::Worker(event) if event.event == "audio")
        }));
    }
}
