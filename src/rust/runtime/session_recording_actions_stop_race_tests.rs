use super::*;
use crate::runtime::recording_capture::RecordingCapture;

/// Teardown finishes exactly as a stale final check would return false.
struct StopAtDecision {
    inner: RecordingCaptureHandle,
    stop: crate::runtime::supervisor::CaptureStop,
}

impl RecordingCapture for StopAtDecision {
    fn open_for_recording(&self) -> bool {
        self.inner.open_for_recording()
    }
    fn close_for_recording(&self) {
        self.inner.close_for_recording();
    }
    fn stop_requested(&self) -> bool {
        (self.stop)();
        false // The old check read this value before teardown completed.
    }
    fn begin_transcription(&self, begin: &mut dyn FnMut() -> bool) -> bool {
        (self.stop)();
        self.inner.begin_transcription(begin)
    }
}

#[test]
fn completed_teardown_cannot_be_followed_by_a_new_transcription_decision() {
    let _env = env_lock();
    let mut rig = rig(None);
    rig.capture = Arc::new(StopAtDecision {
        inner: Arc::clone(&rig.capture),
        stop: Arc::clone(&rig.capture_stop),
    });
    let (mut sink, _rx) = direct_sink(&rig, LiveEnvOverrides::default(), false);
    sink(CoordinatorAction::StartRecording(1));
    assert!(rig.opener.feed(one_second()));
    sink(CoordinatorAction::StopAndTranscribe(1));
    assert!(
        rig.seen.lock().unwrap().is_empty(),
        "completed teardown must prevent STT"
    );
    assert_eq!(rig.session.lock().unwrap().state(), SessionState::Idle);
    assert_eq!(rig.opener.open_streams(), 0);
}
