//! Lifecycle tests for the push-to-talk microphone (#323), driven through a
//! fake opener so they need no audio hardware.

use std::sync::Arc;
use std::time::Duration;

use super::super::capture_test_support::{
    lifecycle_with, statuses, stderr_lines, wait_until, FakeFailure, FakeOpener, RecordingFrames,
    ReporterRig,
};
use super::super::recording_capture::RecordingCapture;
use super::CaptureLifecycle;
use crate::audio::PipelineEvent;

type Fixture = (
    FakeOpener,
    Arc<RecordingFrames>,
    CaptureLifecycle<FakeOpener, RecordingFrames>,
    ReporterRig,
);

fn setup(device: &str) -> Fixture {
    let opener = FakeOpener::default();
    let frames = Arc::new(RecordingFrames::default());
    let (lifecycle, rig) = lifecycle_with(&opener, Arc::clone(&frames), device);
    (opener, frames, lifecycle, rig)
}

#[test]
fn runtime_start_validates_the_device_without_opening_it() {
    let (opener, _frames, _lifecycle, rig) = setup("USB mic");
    assert_eq!(opener.probed(), ["USB mic"]);
    assert!(
        opener.opened().is_empty(),
        "runtime start must not open capture"
    );
    assert_eq!(opener.open_streams(), 0);
    assert!(rig.drain().is_empty());
    assert_eq!(rig.effective_device(), "USB mic");
}

#[test]
fn opens_exactly_once_per_recording_and_closes_when_it_ends() {
    let (opener, _frames, lifecycle, _rig) = setup("USB mic");

    assert!(lifecycle.open_for_recording());
    assert_eq!(opener.open_streams(), 1);
    assert!(
        lifecycle.open_for_recording(),
        "already open counts as open"
    );
    assert_eq!(opener.opened(), ["USB mic"], "a duplicate open is ignored");
    lifecycle.close_for_recording();
    assert_eq!(opener.open_streams(), 0, "idle after the recording");

    assert!(lifecycle.open_for_recording());
    lifecycle.close_for_recording();
    assert_eq!(opener.opened(), ["USB mic", "USB mic"]);
    assert_eq!(opener.open_streams(), 0);
    lifecycle.close_for_recording();
    assert_eq!(opener.open_streams(), 0, "closing twice is harmless");
}

#[test]
fn first_frames_and_frames_up_to_close_reach_the_session() {
    let (opener, frames, lifecycle, _rig) = setup("");
    opener.deliver_on_open(vec![1.0]);
    opener.deliver_on_open(vec![2.0]);

    assert!(lifecycle.open_for_recording());
    assert!(opener.feed(PipelineEvent::Frame(vec![3.0])));
    lifecycle.close_for_recording();

    assert_eq!(
        frames.frames(),
        vec![vec![1.0], vec![2.0], vec![3.0]],
        "close must hand over every delivered frame before transcription"
    );
}

#[test]
fn frames_arriving_while_the_session_is_busy_are_not_dropped() {
    let (opener, frames, lifecycle, _rig) = setup("");
    frames.set_busy(true);
    assert!(lifecycle.open_for_recording());
    assert!(opener.feed(PipelineEvent::Frame(vec![1.0])));
    assert!(opener.feed(PipelineEvent::Frame(vec![2.0])));
    wait_until("forwarder delivery attempts", || frames.attempts() >= 2);
    frames.set_busy(false);
    lifecycle.close_for_recording();
    assert_eq!(frames.frames(), vec![vec![1.0], vec![2.0]]);
}

#[test]
fn runtime_stop_closes_the_stream_and_refuses_later_opens() {
    let (opener, _frames, lifecycle, _rig) = setup("USB mic");
    assert!(lifecycle.open_for_recording());
    assert_eq!(opener.open_streams(), 1);

    (lifecycle.capture_stop())();
    assert_eq!(opener.open_streams(), 0, "stop closes immediately");

    assert!(!lifecycle.open_for_recording());
    assert_eq!(opener.opened().len(), 1, "no open after runtime stop");
    assert_eq!(opener.open_streams(), 0);
    lifecycle.close_for_recording();
}

#[test]
fn dropping_the_lifecycle_closes_an_open_stream() {
    let (opener, _frames, lifecycle, _rig) = setup("USB mic");
    assert!(lifecycle.open_for_recording());
    assert_eq!(opener.open_streams(), 1);
    drop(lifecycle);
    assert_eq!(opener.open_streams(), 0);
}

#[test]
fn runtime_stop_during_a_slow_open_closes_the_new_stream() {
    let (opener, _frames, lifecycle, _rig) = setup("");
    let stop = lifecycle.capture_stop();
    opener.on_open(move || stop());

    assert!(!lifecycle.open_for_recording());
    assert_eq!(opener.opened(), [""]);
    assert_eq!(
        opener.open_streams(),
        0,
        "a stop that wins the race closes it"
    );
    assert!(!lifecycle.open_for_recording());
    assert_eq!(opener.opened().len(), 1);
}

#[test]
fn open_error_keeps_the_device_closed_and_reports_it() {
    let (opener, _frames, lifecycle, rig) = setup("USB mic");
    opener.fail("USB mic", FakeFailure::Error);
    opener.fail("", FakeFailure::Error);

    assert!(!lifecycle.open_for_recording());
    assert_eq!(opener.open_streams(), 0);
    let reported = statuses(&rig.drain());
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].0, "error");
    assert_eq!(reported[0].1["reason"], "device_unusable");
    let error = reported[0].1["error"].as_str().unwrap();
    assert!(error.contains("next push-to-talk press"), "{error}");
    assert_eq!(rig.effective_device(), "");
    lifecycle.close_for_recording();

    opener.clear_failure("USB mic");
    assert!(lifecycle.open_for_recording(), "the next press retries");
    assert_eq!(opener.open_streams(), 1);
    let reported = statuses(&rig.drain());
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].0, "audio-recovered");
    assert_eq!(reported[0].1["audio_device"], "USB mic");
    assert_eq!(rig.effective_device(), "USB mic");
    lifecycle.close_for_recording();
    assert_eq!(opener.opened(), ["USB mic", "", "USB mic"]);
}

#[test]
fn configured_failure_uses_default_for_that_recording_and_retries_next_press() {
    let (opener, _frames, lifecycle, rig) = setup("USB mic");
    opener.fail("USB mic", FakeFailure::Error);

    assert!(lifecycle.open_for_recording());
    assert_eq!(opener.open_streams(), 1);
    lifecycle.close_for_recording();
    let reported = statuses(&rig.drain());
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].0, "audio-fallback");
    assert_eq!(reported[0].1["audio_device"], "System default");
    assert_eq!(rig.effective_device(), "System default");

    assert!(lifecycle.open_for_recording());
    lifecycle.close_for_recording();
    assert!(
        statuses(&rig.drain()).is_empty(),
        "fallback is announced once"
    );

    opener.clear_failure("USB mic");
    assert!(lifecycle.open_for_recording());
    lifecycle.close_for_recording();
    let reported = statuses(&rig.drain());
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].0, "audio-recovered");
    assert_eq!(reported[0].1["audio_device"], "USB mic");
    assert_eq!(opener.opened(), ["USB mic", "", "USB mic", "", "USB mic"]);
    assert_eq!(opener.open_streams(), 0);
}

#[test]
fn device_error_while_recording_closes_the_device_and_never_reopens() {
    let (opener, _frames, lifecycle, rig) = setup("USB mic");
    assert!(lifecycle.open_for_recording());
    assert!(opener.feed(PipelineEvent::DeviceError("unplugged".to_owned())));

    let mut events = Vec::new();
    wait_until("device error status", || {
        events.extend(rig.drain());
        !statuses(&events).is_empty()
    });
    assert_eq!(
        opener.open_streams(),
        0,
        "the dead stream is closed before the release, so the OS indicator goes off"
    );
    let reported = statuses(&events);
    assert_eq!(reported[0].1["reason"], "device_unusable");
    assert!(reported[0].1["error"]
        .as_str()
        .unwrap()
        .contains("stopped during recording"));
    assert!(stderr_lines(&events)
        .iter()
        .any(|line| line.contains("device error: unplugged")));

    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(opener.opened().len(), 1, "no reopen during the recording");
    lifecycle.close_for_recording();
    assert_eq!(opener.open_streams(), 0);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(opener.opened().len(), 1, "an idle runtime never reopens");
    assert!(rig.drain().is_empty());
}

#[test]
fn reaching_max_record_closes_the_microphone_until_the_recording_ends() {
    let (opener, frames, lifecycle, rig) = setup("");
    frames.set_capacity(2);
    assert!(lifecycle.open_for_recording());
    for value in [1.0, 2.0, 3.0] {
        let _ = opener.feed(PipelineEvent::Frame(vec![value]));
    }

    let mut events = Vec::new();
    wait_until("cap closes the microphone", || {
        events.extend(rig.drain());
        opener.open_streams() == 0
            && stderr_lines(&events)
                .iter()
                .any(|line| line.contains("max_record_s"))
    });
    assert_eq!(frames.frames(), vec![vec![1.0], vec![2.0]]);
    assert!(
        statuses(&events).is_empty(),
        "reaching the cap is not a device error"
    );

    lifecycle.close_for_recording();
    assert!(
        lifecycle.open_for_recording(),
        "the next recording opens normally"
    );
    lifecycle.close_for_recording();
    assert_eq!(opener.opened().len(), 2);
}

#[test]
fn timed_out_selectors_get_one_retry_and_are_then_skipped() {
    let (opener, _frames, lifecycle, rig) = setup("USB mic");
    opener.fail("USB mic", FakeFailure::Timeout);
    for _ in 0..3 {
        assert!(lifecycle.open_for_recording());
        lifecycle.close_for_recording();
    }
    assert_eq!(opener.opened(), ["USB mic", "", "USB mic", "", ""]);

    opener.fail("", FakeFailure::Timeout);
    assert!(!lifecycle.open_for_recording());
    let retry = statuses(&rig.drain()).pop().unwrap();
    assert!(retry.1["error"]
        .as_str()
        .unwrap()
        .contains("next push-to-talk press"));
    assert!(!lifecycle.open_for_recording());
    assert!(!lifecycle.open_for_recording());
    assert_eq!(opener.opened(), ["USB mic", "", "USB mic", "", "", "", ""]);
    let paused = statuses(&rig.drain()).pop().unwrap();
    assert_eq!(paused.1["reason"], "device_unusable");
    assert!(paused.1["error"]
        .as_str()
        .unwrap()
        .contains("Restart the dictation runtime"));
}

#[test]
fn startup_with_missing_configured_input_opens_nothing_and_notes_the_default() {
    let opener = FakeOpener::default();
    opener.missing_at_startup("USB mic");
    let (lifecycle, rig) = lifecycle_with(&opener, Arc::new(RecordingFrames::default()), "USB mic");
    assert_eq!(opener.probed(), ["USB mic", ""]);
    assert!(opener.opened().is_empty());
    let events = rig.drain();
    assert!(statuses(&events).is_empty());
    assert!(stderr_lines(&events)[0].contains("not found at startup"));
    assert_eq!(rig.effective_device(), "System default");

    assert!(lifecycle.open_for_recording());
    lifecycle.close_for_recording();
    assert_eq!(
        statuses(&rig.drain())[0].0,
        "audio-recovered",
        "the configured mic came back before the first press"
    );
}

#[test]
fn startup_without_any_input_starts_and_reports_it_on_press() {
    let opener = FakeOpener::default();
    opener.missing_at_startup("USB mic");
    opener.missing_at_startup("");
    opener.fail("USB mic", FakeFailure::Error);
    opener.fail("", FakeFailure::Error);
    let (lifecycle, rig) = lifecycle_with(&opener, Arc::new(RecordingFrames::default()), "USB mic");

    assert!(opener.opened().is_empty());
    let events = rig.drain();
    let reported = statuses(&events);
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].1["reason"], "device_unusable");
    assert!(reported[0].1["error"]
        .as_str()
        .unwrap()
        .contains("No microphone was found"));
    assert!(stderr_lines(&events)[0].contains("no input device found at startup"));
    assert_eq!(rig.effective_device(), "");

    assert!(!lifecycle.open_for_recording());
    assert_eq!(opener.open_streams(), 0);
    assert_eq!(statuses(&rig.drain())[0].1["reason"], "device_unusable");

    opener.clear_failure("");
    assert!(
        lifecycle.open_for_recording(),
        "a device plugged in later works"
    );
    assert_eq!(statuses(&rig.drain())[0].0, "audio-fallback");
    lifecycle.close_for_recording();
}

#[test]
fn slow_open_is_surfaced_on_the_runtime_channel() {
    let (_opener, _frames, lifecycle, rig) = setup("");
    let lifecycle = lifecycle.with_slow_open_warning(Duration::ZERO);
    assert!(lifecycle.open_for_recording());
    lifecycle.close_for_recording();
    assert!(stderr_lines(&rig.drain())
        .iter()
        .any(|line| line.contains("to open") && line.contains("was not captured")));
}

#[test]
fn fast_open_is_silent_on_the_runtime_channel() {
    let (_opener, _frames, lifecycle, rig) = setup("USB mic");
    assert!(lifecycle.open_for_recording());
    lifecycle.close_for_recording();
    assert!(rig.drain().is_empty());
}
