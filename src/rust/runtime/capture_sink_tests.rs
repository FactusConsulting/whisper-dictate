//! Coordinator/sink integration tests for the push-to-talk microphone
//! lifecycle (#323): the device is closed while idle, opened once per
//! utterance, kept open through `release_tail_ms`, and closed before
//! transcription starts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use super::capture_forwarder::SessionFrameSink;
use super::capture_test_support::{lifecycle_with, wait_until, FakeOpener};
use super::live_settings::LiveEnvOverrides;
use super::recording_capture::RecordingCaptureHandle;
use super::rust_session_sink::build_session_action_sink_with_live_overrides;
use crate::audio::PipelineEvent;
use crate::dictate::{
    DictateSession, InjectBackend, InjectError, SessionConfig, TranscribeBackend, TranscribeError,
    TranscribeResult,
};
use crate::hotkey::coordinator::{
    spawn as spawn_coordinator, CoordinatorAction, CoordinatorEvent, CoordinatorHandle,
    CoordinatorThread, Mode, Options,
};

const SR: usize = crate::dictate::session::SR as usize;

/// Records `(pcm_len, open_streams)` when transcription starts, optionally
/// blocking until the test releases it.
#[derive(Clone)]
struct ProbeTranscribe {
    opener: FakeOpener,
    seen: Arc<Mutex<Vec<(usize, usize)>>>,
    gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
}

impl TranscribeBackend for ProbeTranscribe {
    fn transcribe(
        &self,
        pcm: &[f32],
        _sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        self.seen
            .lock()
            .unwrap()
            .push((pcm.len(), self.opener.open_streams()));
        if let Some(gate) = self.gate.as_ref() {
            let _ = gate.lock().unwrap().recv();
        }
        Ok(TranscribeResult {
            text: String::new(),
            gate: Some("capture-lifecycle-probe".to_owned()),
            ..Default::default()
        })
    }
}

struct NoopInject;

impl InjectBackend for NoopInject {
    fn inject(&self, _text: &str) -> Result<(), InjectError> {
        Ok(())
    }
}

type ProbeSession = DictateSession<ProbeTranscribe, NoopInject>;

struct Rig {
    opener: FakeOpener,
    session: Arc<Mutex<ProbeSession>>,
    capture: RecordingCaptureHandle,
    seen: Arc<Mutex<Vec<(usize, usize)>>>,
}

fn rig(gate: Option<mpsc::Receiver<()>>) -> Rig {
    let opener = FakeOpener::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let transcribe = ProbeTranscribe {
        opener: opener.clone(),
        seen: Arc::clone(&seen),
        gate: gate.map(|gate| Arc::new(Mutex::new(gate))),
    };
    let session = Arc::new(Mutex::new(DictateSession::new(
        transcribe,
        NoopInject,
        SessionConfig::default(),
    )));
    let frames = Arc::new(SessionFrameSink::new(Arc::clone(&session)));
    let (lifecycle, _reporter) = lifecycle_with(&opener, frames, "").unwrap();
    Rig {
        opener,
        session,
        capture: Arc::new(lifecycle),
        seen,
    }
}

fn env_lock() -> MutexGuard<'static, ()> {
    crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn one_second() -> PipelineEvent {
    PipelineEvent::Frame(vec![0.1; SR])
}

fn direct_sink(
    rig: &Rig,
    overrides: LiveEnvOverrides,
    runtime_boundaries: bool,
) -> impl FnMut(CoordinatorAction) {
    let (tx, _rx) = mpsc::channel();
    build_session_action_sink_with_live_overrides(
        Arc::clone(&rig.session),
        tx,
        |_| {},
        None,
        overrides,
        runtime_boundaries,
        Some(Arc::clone(&rig.capture)),
    )
}

fn coordinator(rig: &Rig, mode: Mode) -> (CoordinatorHandle, CoordinatorThread) {
    let (tx, _rx) = mpsc::channel();
    let slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());
    let slot_for_sink = Arc::clone(&slot);
    let sink = build_session_action_sink_with_live_overrides(
        Arc::clone(&rig.session),
        tx,
        move |id| {
            if let Some(handle) = slot_for_sink.get() {
                handle.send(CoordinatorEvent::ProcessingFinished(id));
            }
        },
        None,
        LiveEnvOverrides::default(),
        false,
        Some(Arc::clone(&rig.capture)),
    );
    let (handle, thread) = spawn_coordinator(
        Options {
            mode,
            auto_complete_processing: false,
        },
        sink,
        Instant::now,
    );
    assert!(slot.set(handle.clone()).is_ok());
    (handle, thread)
}

#[test]
fn press_opens_and_release_closes_the_microphone_before_transcription() {
    let _env = env_lock();
    let rig = rig(None);
    let mut sink = direct_sink(&rig, LiveEnvOverrides::default(), false);
    assert_eq!(rig.opener.open_streams(), 0, "idle runtime holds no mic");

    sink(CoordinatorAction::StartRecording(1));
    assert_eq!(rig.opener.open_streams(), 1);
    assert!(rig.opener.feed(one_second()));
    sink(CoordinatorAction::StopAndTranscribe(1));

    assert_eq!(*rig.seen.lock().unwrap(), vec![(SR, 0)]);
    assert_eq!(rig.opener.open_streams(), 0);
    assert_eq!(rig.opener.opened(), [""]);
}

#[test]
fn release_tail_is_captured_before_the_microphone_closes() {
    struct RestoreEnv(Vec<(String, Option<std::ffi::OsString>)>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    let _env = env_lock();
    let _restore = RestoreEnv(
        crate::config::effective_live_runtime_settings()
            .into_values()
            .map(|(name, _value, _configured)| {
                let value = std::env::var_os(&name);
                (name, value)
            })
            .collect(),
    );
    let rig = rig(None);
    let overrides = LiveEnvOverrides {
        forced: std::collections::BTreeMap::from([(
            "VOICEPI_RELEASE_TAIL_MS".to_owned(),
            "250".to_owned(),
        )]),
        ..Default::default()
    };
    let mut sink = direct_sink(&rig, overrides, true);
    sink(CoordinatorAction::StartRecording(1));
    assert!(rig.opener.feed(one_second()));

    let tail_opener = rig.opener.clone();
    let tail = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        let open = tail_opener.open_streams();
        let fed = tail_opener.feed(PipelineEvent::Frame(vec![0.2; 480]));
        (open, fed)
    });
    sink(CoordinatorAction::StopAndTranscribe(1));
    let (open_during_tail, fed) = tail.join().unwrap();

    assert_eq!(open_during_tail, 1, "the mic stays open during the tail");
    assert!(fed);
    assert_eq!(*rig.seen.lock().unwrap(), vec![(SR + 480, 0)]);
    assert_eq!(rig.opener.open_streams(), 0);
}

#[test]
fn cancel_closes_the_microphone_and_discards_audio() {
    let _env = env_lock();
    let rig = rig(None);
    let mut sink = direct_sink(&rig, LiveEnvOverrides::default(), false);
    sink(CoordinatorAction::StartRecording(1));
    assert!(rig.opener.feed(one_second()));
    sink(CoordinatorAction::CancelRecording(1));

    assert_eq!(rig.opener.open_streams(), 0);
    assert!(rig.seen.lock().unwrap().is_empty());
    assert_eq!(
        rig.session.lock().unwrap().state(),
        crate::dictate::SessionState::Idle
    );
}

#[test]
fn toggle_mode_keeps_the_microphone_open_until_the_second_press() {
    let _env = env_lock();
    let rig = rig(None);
    let (coord, thread) = coordinator(&rig, Mode::Toggle);

    coord.send(CoordinatorEvent::Press);
    wait_until("toggle-on opens the mic", || rig.opener.open_streams() == 1);
    coord.send(CoordinatorEvent::Release);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(rig.opener.open_streams(), 1, "key-up does not stop toggle");
    assert!(rig.opener.feed(one_second()));

    coord.send(CoordinatorEvent::Press);
    wait_until("toggle-off transcribes", || {
        rig.seen.lock().unwrap().len() == 1
    });
    assert_eq!(rig.seen.lock().unwrap()[0], (SR, 0));
    assert_eq!(rig.opener.open_streams(), 0);
    assert_eq!(rig.opener.opened().len(), 1);

    coord.shutdown();
    thread.join();
}

#[test]
fn press_during_transcription_opens_only_after_transcription_finishes() {
    let _env = env_lock();
    let (gate_tx, gate_rx) = mpsc::channel();
    let rig = rig(Some(gate_rx));
    let (coord, thread) = coordinator(&rig, Mode::HoldToTalk);

    coord.send(CoordinatorEvent::Press);
    wait_until("press opens the mic", || rig.opener.open_streams() == 1);
    assert!(rig.opener.feed(one_second()));
    coord.send(CoordinatorEvent::Release);
    wait_until("transcription started", || {
        rig.seen.lock().unwrap().len() == 1
    });
    assert_eq!(rig.seen.lock().unwrap()[0], (SR, 0));

    coord.send(CoordinatorEvent::Press);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(rig.opener.opened().len(), 1, "no open during transcription");
    assert_eq!(rig.opener.open_streams(), 0);

    gate_tx.send(()).unwrap();
    let opens = AtomicUsize::new(0);
    wait_until("held press replays into a fresh recording", || {
        opens.store(rig.opener.opened().len(), Ordering::SeqCst);
        rig.opener.open_streams() == 1
    });
    assert_eq!(opens.load(Ordering::SeqCst), 2);

    coord.send(CoordinatorEvent::Release);
    wait_until("second release closes the mic", || {
        rig.opener.open_streams() == 0
    });

    coord.shutdown();
    thread.join();
}
