use super::{direct_sink, env_lock, one_second, rig, states};
use crate::hotkey::coordinator::CoordinatorAction;
use crate::runtime::live_settings::LiveEnvOverrides;
use crate::runtime::recording_capture::{RecordingCapture, RecordingCaptureHandle};
use std::sync::{mpsc, Arc};
use std::time::Duration;

struct ObservedCapture {
    inner: RecordingCaptureHandle,
    waiting: mpsc::Sender<()>,
}

impl RecordingCapture for ObservedCapture {
    fn open_for_recording(&self) -> bool {
        self.inner.open_for_recording()
    }

    fn close_for_recording(&self) {
        self.inner.close_for_recording();
    }

    fn stop_requested(&self) -> bool {
        let _ = self.waiting.send(());
        self.inner.stop_requested()
    }

    fn begin_transcription(&self, begin: &mut dyn FnMut() -> bool) -> bool {
        self.inner.begin_transcription(begin)
    }
}

#[test]
fn runtime_stop_interrupts_release_tail_and_discards_without_transcribing() {
    let _env = env_lock();
    let config = tempfile::tempdir().unwrap();
    let path = config.path().join("config.json");
    std::fs::write(&path, "{}").unwrap();
    let mut rig = rig(None);
    let (waiting_tx, waiting_rx) = mpsc::channel();
    rig.capture = Arc::new(ObservedCapture {
        inner: Arc::clone(&rig.capture),
        waiting: waiting_tx,
    });
    let (mut sink, rx) = direct_sink(
        &rig,
        LiveEnvOverrides {
            forced: std::collections::BTreeMap::from([(
                "VOICEPI_RELEASE_TAIL_MS".to_owned(),
                "2000".to_owned(),
            )]),
            config_path: Some(path),
            ..Default::default()
        },
        true,
    );
    sink(CoordinatorAction::StartRecording(1));
    assert!(rig.opener.feed(one_second()));
    std::thread::scope(|scope| {
        let (finished_tx, finished_rx) = mpsc::channel();
        scope.spawn(move || {
            sink(CoordinatorAction::StopAndTranscribe(1));
            finished_tx.send(()).unwrap();
        });
        waiting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        (rig.capture_stop)();
        assert_eq!(rig.opener.open_streams(), 0);
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("runtime stop must not wait out the two-second release tail");
    });
    assert!(rig.seen.lock().unwrap().is_empty());
    assert_eq!(
        rig.session.lock().unwrap().state(),
        crate::dictate::SessionState::Idle
    );
    assert!(states(&rx).iter().any(|state| state == "cancelled"));
}
