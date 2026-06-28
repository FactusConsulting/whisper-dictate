//! Round-7 follow-up tests for [`crate::dictate::audio_route::AudioRoute`].
//!
//! Carved out of `audio_route_tests.rs` to keep both files under the
//! AGENTS.md ~500 LOC modularity bar (the parent file was already
//! pushing the limit with the round 1-6 coverage; the round 7 follow-
//! ups would have tipped it over). Each `#[test]` here corresponds to
//! one of the four Codex P2 findings on round 7 of #415:
//!
//! * round 7-A (audio_route.rs:368) -- `fence_pending_frames` actually
//!   drains.
//! * round 7-C (audio_route.rs:293) -- `start_recording` only refreshes
//!   `RouteConfig` on the success path.
//! * round 7-D (audio_route.rs:250) -- `min_record_seconds` live-reload
//!   reaches the session.

use crate::audio::PipelineEvent;
use crate::dictate::audio_route::RouteError;
use crate::dictate::audio_route_test_support::{
    route_with_cap, start_recording_with_cap_env, EnvVarGuard,
};
use crate::dictate::session::UtteranceOutcome;
use crate::test_env_lock::ENV_LOCK;

/// Codex P2 #415 audio_route.rs:293 (round 7-C): a duplicate
/// `start_recording` (e.g. PTT key-repeat) that hits
/// `SessionError::AlreadyActive` MUST NOT mutate the in-flight
/// recording's cap mid-utterance. Refresh the config from env ONLY on
/// the success path so the documented "between recordings" reload
/// contract is preserved.
#[test]
fn duplicate_start_recording_does_not_refresh_cap_mid_utterance() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();

    // First start: cap = 0.36 s (so the first 12 frames fit, the 13th trips).
    let (_id, _cap_guard) = start_recording_with_cap_env(&mut route, &mut buf, 0.36);
    buf.clear();

    // Flip the env to a much SMALLER cap that, if applied, would
    // trip after a single 480-sample frame (0.03 s).
    let _new_cap = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0.03");

    // Duplicate start MUST be refused -- session is already Recording.
    let err = route
        .start_recording(&mut buf)
        .expect_err("duplicate start_recording must surface AlreadyActive");
    assert!(
        matches!(
            err,
            RouteError::Session(crate::dictate::SessionError::AlreadyActive { .. })
        ),
        "expected RouteError::Session(AlreadyActive), got {err:?}",
    );
    buf.clear();

    // Push 12 frames (= 5760 samples = exactly the ORIGINAL 0.36 s cap).
    // If the refused start had refreshed the cap to 0.03 s, the FIRST
    // over-cap frame here (sample 480 -> 0.03 s) would trip cap_tripped.
    for _ in 0..12 {
        route
            .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
            .expect("under-cap frame must not error");
    }
    assert!(
        !route.cap_tripped(),
        "in-flight 0.36 s cap MUST survive the refused start_recording",
    );
    assert_eq!(route.buffered_samples(), 5760);

    // 13th trips the ORIGINAL cap.
    route
        .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
        .expect("trip frame must not error");
    assert!(
        route.cap_tripped(),
        "original 0.36 s cap must trip on frame 13"
    );
}

/// Codex P2 #415 audio_route.rs:368 (round 7-A): `fence_pending_frames`
/// must actually discard stale events the supervisor has not drained
/// from the pipeline receiver. The route does not own the channel, so
/// the API takes a drain callback. Feed it a sequence of frames + a
/// SpeechEnd, assert every event is consumed, then start a fresh
/// recording and verify ONLY the post-fence frames land in the buffer.
#[test]
fn fence_pending_frames_drains_stale_events_and_keeps_them_off_new_recording() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let _cap_env = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0");
    // Pin MIN_RECORD floor to 0 so the 480-sample (0.03 s) post-fence
    // frame can reach the transcribe backend. The skip helper still
    // clamps the effective floor to 0.3 s as an absolute misfire
    // floor, so we relax the transcribe assertion below to only check
    // buffered_samples; the stale vs. post-fence partition is what
    // round 7-A actually fixes.
    let _min_env = EnvVarGuard::set("VOICEPI_MIN_RECORD_SECONDS", "0");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();

    // Simulate the supervisor's receiver holding 3 stale frames from
    // recording A plus a stray SpeechEnd from A's VAD tail.
    let mut queue: std::collections::VecDeque<PipelineEvent> = std::collections::VecDeque::new();
    queue.push_back(PipelineEvent::Frame(vec![0.1; 480]));
    queue.push_back(PipelineEvent::Frame(vec![0.2; 480]));
    queue.push_back(PipelineEvent::Frame(vec![0.3; 480]));
    queue.push_back(PipelineEvent::SpeechEnd);

    assert_eq!(route.fences_run(), 0);
    let drained = route.fence_pending_frames(|| queue.pop_front());
    assert_eq!(
        drained, 4,
        "fence must drain every event the callback yields; got {drained}",
    );
    assert!(
        queue.is_empty(),
        "drain callback must have been called until None"
    );
    assert_eq!(
        route.fences_run(),
        1,
        "fences_run must increment so callers can confirm the fence ran",
    );

    // Start recording B and push a single frame. ONLY that frame lands
    // in the buffer -- the stale frames from A were discarded.
    route.start_recording(&mut buf).expect("start B");
    route
        .on_event(PipelineEvent::Frame(vec![0.9; 480]), &mut buf)
        .expect("B frame");
    assert_eq!(
        route.buffered_samples(),
        480,
        "post-fence recording must only see its own frames",
    );

    // Stop the recording. The stale frames from A do not reach the
    // transcribe backend -- either skipped (0.03 s is below the
    // skip-helper's 0.3 s absolute floor) or transcribed as exactly
    // 480 samples. Both outcomes prove the fence partition; what the
    // round 7-A regression would surface is the transcriber seeing
    // MORE than 480 samples (the 3 stale frames appended in).
    route.stop_recording(&mut buf).expect("stop B");
    let pcm_lens: Vec<usize> = route
        .session()
        .transcribe_backend()
        .seen_pcm_len
        .borrow()
        .clone();
    for &len in &pcm_lens {
        assert_eq!(
            len, 480,
            "transcribe backend must NEVER see stale-A samples appended onto B's buffer; got {pcm_lens:?}",
        );
    }
}

/// Codex P2 #415 audio_route.rs:368 (round 7-A): an empty queue is a
/// valid fence state -- the supervisor still bumps `fences_run` so
/// downstream telemetry can tell the difference between "no fence was
/// requested" and "fence ran, queue was already clean".
#[test]
fn fence_pending_frames_on_empty_queue_still_bumps_counter() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let _cap_env = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0");
    let mut route = route_with_cap(None);

    let drained = route.fence_pending_frames(|| None);
    assert_eq!(drained, 0);
    assert_eq!(route.fences_run(), 1);
}

/// Codex P2 #415 audio_route.rs:250 (round 7-D): `start_recording`
/// must mirror the freshly-read `VOICEPI_MIN_RECORD_SECONDS` into the
/// session's `SessionConfig.min_record_seconds` so a Settings save
/// between PTT presses takes effect on the next recording without
/// rebuilding the route.
///
/// Construct route, start a recording with a generous floor (2.0 s);
/// a 0.4 s clip skips as too-short. Lower the env to 0.1 s, start a
/// second recording, push the same 0.4 s clip, and assert it
/// transcribes (the skip helper still clamps the effective floor up
/// to 0.3 s, so 0.4 s is above it).
#[test]
fn start_recording_refreshes_min_record_seconds_into_session() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let _cap_env = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();

    // Recording A: floor = 2.0 s. A 0.4 s clip must skip as too_short.
    {
        let _e = EnvVarGuard::set("VOICEPI_MIN_RECORD_SECONDS", "2.0");
        route.start_recording(&mut buf).expect("start A");
        // 0.4 s = 6400 samples at 16 kHz.
        route
            .on_event(PipelineEvent::Frame(vec![0.5; 6400]), &mut buf)
            .expect("A frame");
        let outcome_a = route.stop_recording(&mut buf).expect("stop A");
        assert!(
            matches!(
                outcome_a,
                UtteranceOutcome::Skipped {
                    reason: "too_short"
                },
            ),
            "with 2.0 s floor, a 0.4 s clip must skip; got {outcome_a:?}",
        );
    }

    // Recording B: floor lowered to 0.1 s (skip helper still raises to
    // 0.3 s). The same 0.4 s clip is now above the floor and must
    // transcribe.
    {
        let _e = EnvVarGuard::set("VOICEPI_MIN_RECORD_SECONDS", "0.1");
        route.start_recording(&mut buf).expect("start B");
        route
            .on_event(PipelineEvent::Frame(vec![0.5; 6400]), &mut buf)
            .expect("B frame");
        let outcome_b = route.stop_recording(&mut buf).expect("stop B");
        assert!(
            matches!(outcome_b, UtteranceOutcome::Injected { .. }),
            "with 0.1 s floor (raised to 0.3 s), a 0.4 s clip must transcribe; got {outcome_b:?}",
        );
    }
}

/// Codex P2 #415 audio_route.rs:250 (round 7-D): `DictateSession::update_min_record_seconds`
/// must propagate the new floor into the session's config so the
/// skip helper sees it on the next `stop_and_transcribe`.
#[test]
fn session_update_min_record_seconds_propagates_to_skip_helper() {
    use crate::dictate::audio_route_test_support::{TestInject, TestTranscribe};
    use crate::dictate::session::{DictateSession, SessionConfig};

    let mut session = DictateSession::new(
        TestTranscribe::new(),
        TestInject::new(),
        SessionConfig {
            min_record_seconds: 5.0,
            ..SessionConfig::default()
        },
    );
    let mut buf = Vec::new();
    session.start(&mut buf).expect("start");
    // 1.0 s = 16000 samples. With floor=5.0 s, this skips.
    session.push_frame(&vec![0.5; 16000]);
    let outcome = session.stop_and_transcribe(&mut buf).expect("stop");
    assert!(
        matches!(
            outcome,
            UtteranceOutcome::Skipped {
                reason: "too_short"
            }
        ),
        "1.0 s clip with 5.0 s floor must skip; got {outcome:?}",
    );

    // Update floor live + run another utterance.
    session.update_min_record_seconds(0.5);
    session.start(&mut buf).expect("start B");
    session.push_frame(&vec![0.5; 16000]);
    let outcome_b = session.stop_and_transcribe(&mut buf).expect("stop B");
    assert!(
        matches!(outcome_b, UtteranceOutcome::Injected { .. }),
        "1.0 s clip with 0.5 s floor must transcribe; got {outcome_b:?}",
    );
}
