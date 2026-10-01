//! Tests for capture status reporting.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, RwLock};

use super::{audio_status_event, CaptureReporter};
use crate::runtime::RuntimeEvent;

fn reporter(
    device: &str,
) -> (
    CaptureReporter,
    mpsc::Receiver<RuntimeEvent>,
    Arc<RwLock<String>>,
    Arc<AtomicUsize>,
) {
    let (tx, rx) = mpsc::channel();
    let effective = Arc::new(RwLock::new(device.to_owned()));
    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_for_notifier = Arc::clone(&wakes);
    let notifier: crate::runtime::RepaintNotifier = Arc::new(move || {
        wakes_for_notifier.fetch_add(1, Ordering::SeqCst);
    });
    let reporter = CaptureReporter::new(tx, Some(notifier), Arc::clone(&effective));
    (reporter, rx, effective, wakes)
}

#[test]
fn status_event_carries_only_the_supplied_fields() {
    let RuntimeEvent::Worker(event) =
        audio_status_event("audio-fallback", Some("System default"), None, None)
    else {
        panic!("expected a worker status event");
    };
    assert_eq!(event.event, "status");
    assert_eq!(event.state.as_deref(), Some("audio-fallback"));
    assert_eq!(event.payload["event"], "status");
    assert_eq!(event.payload["state"], "audio-fallback");
    assert_eq!(event.payload["audio_device"], "System default");
    assert!(event.payload.get("reason").is_none());
    assert!(event.payload.get("error").is_none());
}

#[test]
fn capture_unavailable_clears_the_effective_device_and_raises_the_banner() {
    let (reporter, rx, effective, wakes) = reporter("USB mic");
    reporter.capture_unavailable("microphone disappeared");

    assert_eq!(*effective.read().unwrap(), "");
    let RuntimeEvent::Worker(event) = rx.recv().unwrap() else {
        panic!("expected a worker status event");
    };
    assert_eq!(event.state.as_deref(), Some("error"));
    assert_eq!(event.payload["reason"], "device_unusable");
    assert_eq!(event.payload["error"], "microphone disappeared");
    assert!(event.payload.get("audio_device").is_none());
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn log_lines_and_device_labels_are_published() {
    let (reporter, rx, effective, wakes) = reporter("USB mic");
    reporter.set_effective_device("System default");
    reporter.stderr("[rust-session-audio] capture opened".to_owned());

    assert_eq!(*effective.read().unwrap(), "System default");
    assert!(matches!(
        rx.recv().unwrap(),
        RuntimeEvent::Stderr(line) if line == "[rust-session-audio] capture opened"
    ));
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn dropped_audio_is_visible_without_changing_the_runtime_status() {
    let (reporter, rx, effective, _) = reporter("USB mic");
    reporter.overflow(
        crate::audio::raw::RawCaptureOverflow {
            capture_chunks: 2,
            pipeline_events: 3,
        },
        5,
    );
    assert!(
        matches!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), RuntimeEvent::Stderr(line) if line.contains("WARNING") && line.contains("transcript may be incomplete"))
    );
    let RuntimeEvent::Worker(event) = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap()
    else {
        panic!("structured loss report required")
    };
    assert_eq!(event.event, "audio_overflow");
    assert_eq!(event.state, None);
    assert_eq!(event.payload["capture_chunks_dropped"], 2);
    assert_eq!(event.payload["pipeline_events_dropped"], 3);
    assert_eq!(event.payload["pending_frames_dropped"], 5);
    assert_eq!(*effective.read().unwrap(), "USB mic");
    assert!(rx.try_recv().is_err());
    reporter.overflow(Default::default(), 0);
    assert!(rx.try_recv().is_err(), "lossless capture must remain quiet");
}
