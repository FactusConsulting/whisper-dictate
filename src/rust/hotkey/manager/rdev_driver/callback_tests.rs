//! Exercise the actual callback without installing an OS keyboard hook.

use super::CallbackContext;
use crate::hotkey::inject_guard::InjectionGuard;
use crate::hotkey::manager::tracker::{KeyTracker, RawKeyEvent, TrackerOutput};
use crate::hotkey::manager::RawTap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

struct TestTap(Arc<Mutex<Vec<RawKeyEvent>>>);

impl RawTap for TestTap {
    fn tap(&self, event: &RawKeyEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

type Sink = Box<dyn Fn(TrackerOutput) + Send + Sync>;

struct Fixture {
    callback: CallbackContext<Sink, TestTap>,
    outputs: Arc<Mutex<Vec<TrackerOutput>>>,
    tapped: Arc<Mutex<Vec<RawKeyEvent>>>,
}

fn fixture() -> Fixture {
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let tapped = Arc::new(Mutex::new(Vec::new()));
    let output_sink = Arc::clone(&outputs);
    Fixture {
        callback: CallbackContext {
            listener_tracker: Arc::new(Mutex::new(KeyTracker::new(vec!["f9".into()]))),
            listener_sink: Arc::new(Box::new(move |output| {
                output_sink.lock().unwrap().push(output);
            })),
            listener_tap: Arc::new(TestTap(Arc::clone(&tapped))),
            listener_guard: Arc::new(InjectionGuard::new()),
            listener_total: Arc::new(AtomicU64::new(0)),
            listener_since: Arc::new(AtomicU64::new(0)),
        },
        outputs,
        tapped,
    }
}

fn event(event_type: rdev::EventType) -> rdev::Event {
    rdev::Event {
        event_type,
        time: SystemTime::UNIX_EPOCH,
        name: None,
    }
}

#[test]
fn mouse_event_does_not_reach_key_counters_tap_or_tracker() {
    let fixture = fixture();
    fixture
        .callback
        .handle_event(event(rdev::EventType::MouseMove { x: 1.0, y: 2.0 }));
    assert_eq!(fixture.callback.listener_total.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.callback.listener_since.load(Ordering::Relaxed), 0);
    assert!(fixture.tapped.lock().unwrap().is_empty());
    assert!(fixture.outputs.lock().unwrap().is_empty());
}

#[test]
fn callback_counts_and_taps_repeats_but_dispatches_only_chord_edges() {
    let fixture = fixture();
    for event_type in [
        rdev::EventType::KeyPress(rdev::Key::F9),
        rdev::EventType::KeyPress(rdev::Key::F9),
        rdev::EventType::KeyRelease(rdev::Key::F9),
    ] {
        fixture.callback.handle_event(event(event_type));
    }
    assert_eq!(fixture.callback.listener_total.load(Ordering::Relaxed), 3);
    assert_eq!(fixture.callback.listener_since.load(Ordering::Relaxed), 3);
    assert_eq!(fixture.tapped.lock().unwrap().len(), 3);
    assert_eq!(
        *fixture.outputs.lock().unwrap(),
        [TrackerOutput::ChordPress, TrackerOutput::ChordRelease]
    );
}

#[test]
fn injection_guard_suppresses_dispatch_not_observation_counters_or_tap() {
    let fixture = fixture();
    fixture.callback.listener_guard.arm_start(Duration::ZERO);
    fixture
        .callback
        .handle_event(event(rdev::EventType::KeyPress(rdev::Key::F9)));
    fixture
        .callback
        .handle_event(event(rdev::EventType::KeyRelease(rdev::Key::F9)));
    assert_eq!(fixture.callback.listener_total.load(Ordering::Relaxed), 2);
    assert_eq!(fixture.callback.listener_since.load(Ordering::Relaxed), 2);
    assert_eq!(fixture.tapped.lock().unwrap().len(), 2);
    assert!(fixture.outputs.lock().unwrap().is_empty());
    fixture.callback.listener_guard.arm_end(Duration::ZERO);
    fixture
        .callback
        .handle_event(event(rdev::EventType::KeyPress(rdev::Key::F9)));
    assert_eq!(
        *fixture.outputs.lock().unwrap(),
        [TrackerOutput::ChordPress]
    );
}
