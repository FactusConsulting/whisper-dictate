//! Coordinator/sink integration tests for the push-to-talk microphone
//! lifecycle (#323): the device is closed while idle, opened once per
//! utterance, announced only once open, kept open through `release_tail_ms`,
//! and closed before transcription starts. Races against a slow open live in
//! the child module `race` (`session_recording_actions_race_tests.rs`), which
//! reuses these helpers.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::audio::PipelineEvent;
use crate::dictate::feedback::{CueKind, CueSink};
use crate::dictate::{
    DictateSession, InjectBackend, InjectError, SessionConfig, SessionState, TranscribeBackend,
    TranscribeError, TranscribeResult,
};
use crate::hotkey::coordinator::{
    spawn as spawn_coordinator, CoordinatorAction, CoordinatorEvent, CoordinatorHandle,
    CoordinatorThread, Mode, Options,
};
use crate::runtime::capture_forwarder::SessionFrameSink;
use crate::runtime::capture_test_support::{lifecycle_with, wait_until, FakeFailure, FakeOpener};
use crate::runtime::live_settings::LiveEnvOverrides;
use crate::runtime::recording_capture::RecordingCaptureHandle;
use crate::runtime::rust_session_sink::{
    build_session_action_sink_with_live_overrides, coordinator_signals, CoordinatorSignals,
};
use crate::runtime::RuntimeEvent;

#[path = "session_recording_actions_race_tests.rs"]
mod race;

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

/// Records `(cue, open_streams)` for every played cue.
struct ProbeCue {
    opener: FakeOpener,
    played: Arc<Mutex<Vec<(CueKind, usize)>>>,
}

impl CueSink for ProbeCue {
    fn play(&self, kind: CueKind) {
        self.played
            .lock()
            .unwrap()
            .push((kind, self.opener.open_streams()));
    }
}

type ProbeSession = DictateSession<ProbeTranscribe, NoopInject>;

struct Rig {
    opener: FakeOpener,
    session: Arc<Mutex<ProbeSession>>,
    capture: RecordingCaptureHandle,
    seen: Arc<Mutex<Vec<(usize, usize)>>>,
    cues: Arc<Mutex<Vec<(CueKind, usize)>>>,
}

fn rig_with(config: SessionConfig, gate: Option<mpsc::Receiver<()>>) -> Rig {
    let opener = FakeOpener::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let cues = Arc::new(Mutex::new(Vec::new()));
    let transcribe = ProbeTranscribe {
        opener: opener.clone(),
        seen: Arc::clone(&seen),
        gate: gate.map(|gate| Arc::new(Mutex::new(gate))),
    };
    let session = DictateSession::new(transcribe, NoopInject, config)
        .with_worker_events_enabled()
        .with_cue_sink(Box::new(ProbeCue {
            opener: opener.clone(),
            played: Arc::clone(&cues),
        }));
    let session = Arc::new(Mutex::new(session));
    let frames = Arc::new(SessionFrameSink::new(Arc::clone(&session)));
    let (lifecycle, _reporter) = lifecycle_with(&opener, frames, "");
    Rig {
        opener,
        session,
        capture: Arc::new(lifecycle),
        seen,
        cues,
    }
}

fn rig(gate: Option<mpsc::Receiver<()>>) -> Rig {
    rig_with(SessionConfig::default(), gate)
}

fn env_lock() -> MutexGuard<'static, ()> {
    crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn one_second() -> PipelineEvent {
    PipelineEvent::Frame(vec![0.1; SR])
}

fn states(rx: &mpsc::Receiver<RuntimeEvent>) -> Vec<String> {
    rx.try_iter()
        .filter_map(|event| match event {
            RuntimeEvent::Worker(worker) => worker.state,
            _ => None,
        })
        .collect()
}

fn direct_sink(
    rig: &Rig,
    overrides: LiveEnvOverrides,
    runtime_boundaries: bool,
) -> (impl FnMut(CoordinatorAction), mpsc::Receiver<RuntimeEvent>) {
    let (tx, rx) = mpsc::channel();
    let sink = build_session_action_sink_with_live_overrides(
        Arc::clone(&rig.session),
        tx,
        CoordinatorSignals {
            processing_finished: |_| {},
            recording_abandoned: |_| {},
        },
        None,
        overrides,
        runtime_boundaries,
        Some(Arc::clone(&rig.capture)),
    );
    (sink, rx)
}

fn coordinator(
    session: &Arc<Mutex<ProbeSession>>,
    capture: RecordingCaptureHandle,
    mode: Mode,
) -> (CoordinatorHandle, CoordinatorThread) {
    let (tx, _rx) = mpsc::channel();
    let slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());
    // The production wiring, so the abandoned-open path is covered end to end.
    let sink = build_session_action_sink_with_live_overrides(
        Arc::clone(session),
        tx,
        coordinator_signals(&slot),
        None,
        LiveEnvOverrides::default(),
        false,
        Some(capture),
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
    let (mut sink, _rx) = direct_sink(&rig, LiveEnvOverrides::default(), false);
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
fn start_cue_and_recording_status_wait_for_the_open_microphone() {
    let _env = env_lock();
    let rig = rig(None);
    let (mut sink, rx) = direct_sink(&rig, LiveEnvOverrides::default(), false);
    let rx = Arc::new(Mutex::new(rx));
    let during_open = Arc::new(Mutex::new(Vec::<String>::new()));
    let (rx_hook, during_hook) = (Arc::clone(&rx), Arc::clone(&during_open));
    let cues_hook = Arc::clone(&rig.cues);
    let cues_during_open = Arc::new(Mutex::new(None));
    let cues_during_hook = Arc::clone(&cues_during_open);
    rig.opener.on_open(move || {
        during_hook
            .lock()
            .unwrap()
            .extend(states(&rx_hook.lock().unwrap()));
        *cues_during_hook.lock().unwrap() = Some(cues_hook.lock().unwrap().len());
    });

    sink(CoordinatorAction::StartRecording(1));

    let during = during_open.lock().unwrap().clone();
    assert!(during.contains(&"opening".to_owned()), "{during:?}");
    assert!(
        !during.contains(&"recording".to_owned()),
        "recording must not be announced before the microphone is open: {during:?}"
    );
    assert_eq!(*cues_during_open.lock().unwrap(), Some(0), "no cue yet");
    let after = states(&rx.lock().unwrap());
    assert!(after.contains(&"recording".to_owned()), "{after:?}");
    assert_eq!(
        *rig.cues.lock().unwrap(),
        vec![(CueKind::Start, 1)],
        "the start cue plays only once capture is live"
    );

    sink(CoordinatorAction::StopAndTranscribe(1));
    assert_eq!(
        *rig.cues.lock().unwrap(),
        vec![(CueKind::Start, 1), (CueKind::Stop, 0)]
    );
}

#[test]
fn failed_open_plays_no_cue_and_returns_the_session_to_ready() {
    let _env = env_lock();
    let rig = rig(None);
    rig.opener.fail("", FakeFailure::Error);
    let (mut sink, rx) = direct_sink(&rig, LiveEnvOverrides::default(), false);

    sink(CoordinatorAction::StartRecording(1));
    assert!(rig.cues.lock().unwrap().is_empty());
    assert_eq!(rig.session.lock().unwrap().state(), SessionState::Idle);
    let seen_states = states(&rx);
    assert!(
        !seen_states.contains(&"recording".to_owned()),
        "{seen_states:?}"
    );
    assert_eq!(
        seen_states.last().map(String::as_str),
        Some("ready"),
        "{seen_states:?}"
    );

    sink(CoordinatorAction::StopAndTranscribe(1));
    assert!(
        rig.cues.lock().unwrap().is_empty(),
        "no stop cue without a start cue"
    );
    assert!(rig.seen.lock().unwrap().is_empty());
    assert_eq!(rig.opener.open_streams(), 0);
}

#[test]
fn refused_session_start_does_not_open_the_microphone() {
    let _env = env_lock();
    let rig = rig(None);
    rig.session
        .lock()
        .unwrap()
        .start(&mut std::io::sink())
        .unwrap();
    let (mut sink, rx) = direct_sink(&rig, LiveEnvOverrides::default(), false);

    sink(CoordinatorAction::StartRecording(2));

    assert!(rig.opener.opened().is_empty());
    assert!(rx.try_iter().any(
        |event| matches!(event, RuntimeEvent::Error(message) if message.contains("start failed"))
    ));
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
    let (mut sink, _rx) = direct_sink(&rig, overrides, true);
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
    let (mut sink, _rx) = direct_sink(&rig, LiveEnvOverrides::default(), false);
    sink(CoordinatorAction::StartRecording(1));
    assert!(rig.opener.feed(one_second()));
    sink(CoordinatorAction::CancelRecording(1));

    assert_eq!(rig.opener.open_streams(), 0);
    assert!(rig.seen.lock().unwrap().is_empty());
    assert_eq!(rig.session.lock().unwrap().state(), SessionState::Idle);
}

#[test]
fn stale_cancel_keeps_a_newer_recording_and_its_microphone() {
    let _env = env_lock();
    let rig = rig(None);
    let (mut sink, _rx) = direct_sink(&rig, LiveEnvOverrides::default(), false);
    sink(CoordinatorAction::StartRecording(1));
    assert_eq!(rig.opener.open_streams(), 1);

    sink(CoordinatorAction::CancelRecording(99));
    assert_eq!(
        rig.opener.open_streams(),
        1,
        "a stale-epoch cancel must not close the live microphone"
    );
    assert_eq!(
        rig.session.lock().unwrap().state(),
        SessionState::Recording { id: 1 },
        "the session keeps the recording the stale cancel did not match"
    );
    assert!(rig.opener.feed(one_second()));

    sink(CoordinatorAction::CancelRecording(1));
    assert_eq!(rig.opener.open_streams(), 0);
    assert_eq!(rig.session.lock().unwrap().state(), SessionState::Idle);
    assert!(rig.seen.lock().unwrap().is_empty());
}

#[test]
fn reaching_max_record_closes_the_microphone_before_the_recording_ends() {
    let _env = env_lock();
    let config = SessionConfig {
        max_record_seconds: Some(1.0),
        ..SessionConfig::default()
    };
    let rig = rig_with(config, None);
    let (coord, thread) = coordinator(&rig.session, Arc::clone(&rig.capture), Mode::Toggle);

    coord.send(CoordinatorEvent::Press);
    wait_until("toggle-on opens the mic", || rig.opener.open_streams() == 1);
    assert!(rig.opener.feed(PipelineEvent::Frame(vec![0.1; 2 * SR])));
    wait_until("the cap closes the mic without a second press", || {
        rig.opener.open_streams() == 0
    });
    assert_eq!(
        rig.session.lock().unwrap().state(),
        SessionState::Recording { id: 1 },
        "the recording itself continues until the toggle press"
    );

    coord.send(CoordinatorEvent::Press);
    wait_until("toggle-off transcribes the capped audio", || {
        rig.seen.lock().unwrap().len() == 1
    });
    assert_eq!(rig.seen.lock().unwrap()[0], (SR, 0));
    assert_eq!(rig.opener.opened().len(), 1);

    coord.shutdown();
    thread.join();
}

#[test]
fn toggle_mode_keeps_the_microphone_open_until_the_second_press() {
    let _env = env_lock();
    let rig = rig(None);
    let (coord, thread) = coordinator(&rig.session, Arc::clone(&rig.capture), Mode::Toggle);

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
    let (coord, thread) = coordinator(&rig.session, Arc::clone(&rig.capture), Mode::HoldToTalk);

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
    wait_until("held press replays into a fresh recording", || {
        rig.opener.open_streams() == 1
    });
    assert_eq!(rig.opener.opened().len(), 2);

    coord.send(CoordinatorEvent::Release);
    wait_until("second release closes the mic", || {
        rig.opener.open_streams() == 0
    });

    coord.shutdown();
    thread.join();
}
