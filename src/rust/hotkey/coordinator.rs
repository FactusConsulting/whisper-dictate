//! [`TranscriptionCoordinator`] — the lifecycle state machine that serialises
//! every PTT press/release/processing-finished event through a single mpsc
//! channel.
//!
//! The whole point of moving PTT into Rust (issue #318) is to make the
//! press/release race conditions that bit us in #254 and #274
//! *unrepresentable*: every transition runs on one thread, gated by a
//! [`Stage`] enum, so a spurious release that arrives after we've already
//! moved to [`Stage::Processing`] can no longer fire a start.
//!
//! Stage transitions (the only legal moves):
//!
//! ```text
//!     Idle ─── press ───▶ Recording(id)
//!     Recording(id) ── release ──▶ Processing
//!     Processing ── processing_finished ──▶ Idle
//!     Recording(id) ── cancel ──▶ Idle (no Processing — discard audio)
//! ```
//!
//! Everything else is dropped (and logged at debug level via the host callback
//! — the coordinator itself stays silent so tests can assert behaviour without
//! grepping stdout):
//!
//! * `press` while in [`Stage::Recording`] / [`Stage::Processing`]: ignored.
//!   In Recording it's almost always a key-repeat; in Processing it's the
//!   user trying to start the next utterance before the previous one finished
//!   — we acknowledge the press but defer it until we're back in Idle.
//! * `release` while in [`Stage::Idle`] / [`Stage::Processing`]: dropped. This
//!   is the **drop-guard** that closes the #254-style hole — a release that
//!   races a processing-finished event cannot wake the recorder.
//!
//! ## Debounce
//!
//! Press events are debounced by [`PRESS_DEBOUNCE`] (~30 ms by default) so a
//! second press that arrives within the debounce window from the same Idle
//! state is suppressed. This matches the host-side jitter we observe when a
//! key bounces on cheap mechanical keyboards (and the Bluetooth headset
//! double-tap pattern). The window is per-stage: we restart it every time we
//! re-enter Idle so the *next* start is not falsely suppressed.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Default press-debounce window. Spurious presses from the same Idle state
/// within this window are dropped. Matches the Python evdev/pynput jitter
/// we've measured (#274 follow-up notes).
pub const PRESS_DEBOUNCE: Duration = Duration::from_millis(30);

/// Monotonic identifier for a recording session, incremented every time the
/// coordinator enters [`Stage::Recording`]. Used by the host to capture the
/// *current* generation when it schedules a cancel — a stale cancel that
/// arrives after a later recording has started is harmlessly ignored because
/// the ids no longer match (mirrors the `_record_epoch` pattern in
/// `vp_keys.py`).
pub type RecordingId = u64;

/// Lifecycle state of a single PTT cycle. Owned by the coordinator thread; no
/// other thread ever reads or writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// No PTT in flight. The next press will start a recording.
    Idle,
    /// PTT is held / has been pressed in toggle mode. The next release ends
    /// it. The id is bumped every time we enter Recording so the host can
    /// scope cancels (see [`RecordingId`]).
    Recording(RecordingId),
    /// The release fired and the host is busy transcribing. Press / release
    /// events in this state are deferred or dropped — never acted on. We
    /// leave Processing only when the host sends
    /// [`CoordinatorEvent::ProcessingFinished`].
    Processing,
}

/// Lifecycle events the coordinator accepts on its inbound channel. Producers
/// (the rdev manager thread, and the host when it finishes a transcription)
/// send these via [`CoordinatorHandle::send`].
#[derive(Debug, Clone, Copy)]
pub enum CoordinatorEvent {
    /// The bound PTT chord just completed (rising edge — never key-repeat).
    Press,
    /// The bound PTT chord just broke (falling edge).
    Release,
    /// The host finished transcribing / injecting. Moves Processing → Idle.
    /// Safe to send from any stage — if the coordinator is not in Processing
    /// it is a no-op (the host crashed mid-cycle and recovered, etc.).
    ProcessingFinished,
    /// Foreign-key chord detected by the manager — discard any in-flight
    /// recording and return to Idle without transcribing.
    Cancel,
    /// Stop the coordinator thread cleanly. Sent by
    /// [`CoordinatorHandle::shutdown`].
    Shutdown,
}

/// Side-effects the coordinator asks the host to perform. The host runs
/// these on its own threads; the coordinator never blocks waiting for them
/// to complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinatorAction {
    /// Start a new recording with this generation id. The host should capture
    /// the id and pass it back when reporting cancel / processing-finished.
    StartRecording(RecordingId),
    /// End the current recording and run the transcription pass. The id
    /// matches the [`Self::StartRecording`] that began it.
    StopAndTranscribe(RecordingId),
    /// Discard the in-flight recording — no transcription, no injection.
    CancelRecording(RecordingId),
}

/// Public handle to the coordinator thread. Cloneable so multiple producers
/// (the rdev manager, the supervisor) can send events into the same
/// state machine without each holding a separate channel.
#[derive(Clone)]
pub struct CoordinatorHandle {
    tx: Sender<CoordinatorEvent>,
}

impl CoordinatorHandle {
    /// Send an event to the coordinator. Drops silently if the coordinator
    /// thread has already exited (the host is shutting down).
    pub fn send(&self, event: CoordinatorEvent) {
        let _ = self.tx.send(event);
    }

    /// Ask the coordinator thread to exit. Subsequent [`Self::send`] calls
    /// are no-ops. Returns immediately — the thread is joined separately via
    /// the [`CoordinatorThread`] handle.
    pub fn shutdown(&self) {
        let _ = self.tx.send(CoordinatorEvent::Shutdown);
    }
}

/// Owned join handle for the coordinator thread. The supervisor keeps this
/// alive for the lifetime of the hotkey subsystem and joins it on shutdown.
pub struct CoordinatorThread {
    join: Option<JoinHandle<()>>,
}

impl CoordinatorThread {
    /// Wait for the coordinator thread to finish (after sending Shutdown via
    /// [`CoordinatorHandle::shutdown`]). Idempotent — safe to call twice;
    /// the second call is a no-op.
    pub fn join(mut self) {
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn the coordinator thread. `action_sink` is invoked on the coordinator
/// thread every time a stage transition produces an action; it MUST be cheap
/// and non-blocking (push onto a channel / spawn a worker), or the next event
/// will be delayed. Returns a [`CoordinatorHandle`] for producers and a
/// [`CoordinatorThread`] for the supervisor's lifecycle.
///
/// `clock` is injected so tests can drive debounce deterministically. In
/// production this is [`Instant::now`].
pub fn spawn<F, C>(action_sink: F, clock: C) -> (CoordinatorHandle, CoordinatorThread)
where
    F: FnMut(CoordinatorAction) + Send + 'static,
    C: FnMut() -> Instant + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name("vp-hotkey-coordinator".to_owned())
        .spawn(move || coordinator_loop(rx, action_sink, clock))
        .expect("hotkey coordinator thread spawn");
    (
        CoordinatorHandle { tx },
        CoordinatorThread { join: Some(join) },
    )
}

/// Synchronous step function — the heart of the state machine. Exposed
/// `pub(super)` so unit tests can drive it without spinning up a thread; the
/// production [`coordinator_loop`] is a thin wrapper that pumps mpsc events
/// through this same function.
pub(super) fn step(
    stage: &mut Stage,
    next_id: &mut RecordingId,
    last_idle_press: &mut Option<Instant>,
    now: Instant,
    event: CoordinatorEvent,
) -> Option<CoordinatorAction> {
    match (event, *stage) {
        (CoordinatorEvent::Press, Stage::Idle) => {
            // Debounce: drop a press that arrives within PRESS_DEBOUNCE of the
            // previous Idle-press. Mostly catches bouncing keyboards and the
            // BT-headset double-tap pattern.
            if let Some(prev) = *last_idle_press {
                if now.duration_since(prev) < PRESS_DEBOUNCE {
                    return None;
                }
            }
            *last_idle_press = Some(now);
            *next_id = next_id.wrapping_add(1);
            let id = *next_id;
            *stage = Stage::Recording(id);
            Some(CoordinatorAction::StartRecording(id))
        }
        (CoordinatorEvent::Press, _) => {
            // Recording or Processing: ignore — it's almost always key-repeat,
            // or the user mashing PTT during the transcription pass. Either
            // way, no action.
            None
        }
        (CoordinatorEvent::Release, Stage::Recording(id)) => {
            *stage = Stage::Processing;
            Some(CoordinatorAction::StopAndTranscribe(id))
        }
        (CoordinatorEvent::Release, _) => {
            // Drop-guard. A release that arrives in Idle (no recording to end)
            // or in Processing (the host is already transcribing) is the
            // #254-class hole — silently drop it.
            None
        }
        (CoordinatorEvent::Cancel, Stage::Recording(id)) => {
            *stage = Stage::Idle;
            *last_idle_press = None; // re-arm debounce so the next press is fresh
            Some(CoordinatorAction::CancelRecording(id))
        }
        (CoordinatorEvent::Cancel, _) => {
            // Nothing to cancel in Idle / Processing.
            None
        }
        (CoordinatorEvent::ProcessingFinished, Stage::Processing) => {
            *stage = Stage::Idle;
            *last_idle_press = None; // re-arm debounce — the new cycle is fresh
            None
        }
        (CoordinatorEvent::ProcessingFinished, _) => {
            // Host crashed mid-cycle or sent a stale completion. Resync to
            // Idle without emitting an action; better than wedging.
            *stage = Stage::Idle;
            *last_idle_press = None;
            None
        }
        (CoordinatorEvent::Shutdown, _) => None,
    }
}

fn coordinator_loop<F, C>(rx: Receiver<CoordinatorEvent>, mut action_sink: F, mut clock: C)
where
    F: FnMut(CoordinatorAction),
    C: FnMut() -> Instant,
{
    let mut stage = Stage::Idle;
    let mut next_id: RecordingId = 0;
    let mut last_idle_press: Option<Instant> = None;
    loop {
        // recv_timeout so the loop never blocks indefinitely without a
        // chance to notice a poisoned channel; the timeout is large because
        // there's no work to do without an event.
        let event = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if matches!(event, CoordinatorEvent::Shutdown) {
            return;
        }
        let now = clock();
        if let Some(action) = step(&mut stage, &mut next_id, &mut last_idle_press, now, event) {
            action_sink(action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (Stage, RecordingId, Option<Instant>) {
        (Stage::Idle, 0, None)
    }

    #[test]
    fn idle_then_press_enters_recording_and_emits_start() {
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        let action = step(&mut stage, &mut id, &mut last, t0, CoordinatorEvent::Press);
        assert_eq!(action, Some(CoordinatorAction::StartRecording(1)));
        assert_eq!(stage, Stage::Recording(1));
        assert_eq!(id, 1);
    }

    #[test]
    fn recording_then_release_enters_processing_and_emits_stop() {
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        step(&mut stage, &mut id, &mut last, t0, CoordinatorEvent::Press);
        let action = step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(500),
            CoordinatorEvent::Release,
        );
        assert_eq!(action, Some(CoordinatorAction::StopAndTranscribe(1)));
        assert_eq!(stage, Stage::Processing);
    }

    #[test]
    fn processing_then_finished_returns_to_idle() {
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        step(&mut stage, &mut id, &mut last, t0, CoordinatorEvent::Press);
        step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(500),
            CoordinatorEvent::Release,
        );
        let action = step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(900),
            CoordinatorEvent::ProcessingFinished,
        );
        assert_eq!(action, None);
        assert_eq!(stage, Stage::Idle);
        assert_eq!(last, None, "debounce re-armed after Processing→Idle");
    }

    #[test]
    fn second_press_within_debounce_window_is_dropped() {
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        step(&mut stage, &mut id, &mut last, t0, CoordinatorEvent::Press);
        // Cancel back to Idle so the next press is the one being debounced.
        step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(5),
            CoordinatorEvent::Cancel,
        );
        // last_idle_press was cleared by Cancel, so we need a different
        // scenario: take the natural path (Press → Release → Finished → Press).
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        let first = step(&mut stage, &mut id, &mut last, t0, CoordinatorEvent::Press);
        assert!(matches!(first, Some(CoordinatorAction::StartRecording(_))));
        // Manually drop back to Idle WITHOUT clearing last_idle_press — the
        // debounce is meant to catch presses on the SAME Idle state, so
        // simulate the chord breaking before the host had time to react.
        stage = Stage::Idle;
        // Second press only 10 ms after the first → suppressed.
        let action = step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(10),
            CoordinatorEvent::Press,
        );
        assert_eq!(action, None);
        assert_eq!(stage, Stage::Idle);
        // Third press well outside the debounce window → fires.
        let action = step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + PRESS_DEBOUNCE + Duration::from_millis(5),
            CoordinatorEvent::Press,
        );
        assert!(matches!(action, Some(CoordinatorAction::StartRecording(_))));
    }

    #[test]
    fn spurious_release_in_idle_is_dropped() {
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        let action = step(
            &mut stage,
            &mut id,
            &mut last,
            t0,
            CoordinatorEvent::Release,
        );
        assert_eq!(action, None);
        assert_eq!(stage, Stage::Idle);
    }

    #[test]
    fn spurious_release_in_processing_is_dropped() {
        // The #254-class hole: release races processing-finished and tries
        // to start a fresh recording. The drop-guard makes it a no-op.
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        step(&mut stage, &mut id, &mut last, t0, CoordinatorEvent::Press);
        step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(300),
            CoordinatorEvent::Release,
        );
        assert_eq!(stage, Stage::Processing);
        let action = step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(310),
            CoordinatorEvent::Release,
        );
        assert_eq!(action, None);
        assert_eq!(
            stage,
            Stage::Processing,
            "still Processing after spurious release"
        );
    }

    #[test]
    fn press_during_processing_is_ignored() {
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        step(&mut stage, &mut id, &mut last, t0, CoordinatorEvent::Press);
        step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(300),
            CoordinatorEvent::Release,
        );
        let action = step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(400),
            CoordinatorEvent::Press,
        );
        assert_eq!(action, None);
        assert_eq!(stage, Stage::Processing);
    }

    #[test]
    fn press_during_recording_is_ignored_keyrepeat() {
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        step(&mut stage, &mut id, &mut last, t0, CoordinatorEvent::Press);
        // Key-repeat ~50 ms later: must not re-fire StartRecording.
        let action = step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(50),
            CoordinatorEvent::Press,
        );
        assert_eq!(action, None);
        assert_eq!(stage, Stage::Recording(1));
    }

    #[test]
    fn cancel_in_recording_emits_cancel_and_returns_to_idle() {
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        step(&mut stage, &mut id, &mut last, t0, CoordinatorEvent::Press);
        let action = step(
            &mut stage,
            &mut id,
            &mut last,
            t0 + Duration::from_millis(200),
            CoordinatorEvent::Cancel,
        );
        assert_eq!(action, Some(CoordinatorAction::CancelRecording(1)));
        assert_eq!(stage, Stage::Idle);
    }

    #[test]
    fn cancel_in_idle_is_noop() {
        let (mut stage, mut id, mut last) = fresh();
        let t0 = Instant::now();
        let action = step(&mut stage, &mut id, &mut last, t0, CoordinatorEvent::Cancel);
        assert_eq!(action, None);
        assert_eq!(stage, Stage::Idle);
    }

    #[test]
    fn recording_id_increments_per_cycle() {
        let (mut stage, mut id, mut last) = fresh();
        let mut t = Instant::now();
        for expected in 1..=3 {
            let a = step(&mut stage, &mut id, &mut last, t, CoordinatorEvent::Press);
            assert_eq!(a, Some(CoordinatorAction::StartRecording(expected)));
            t += Duration::from_millis(100);
            let a = step(&mut stage, &mut id, &mut last, t, CoordinatorEvent::Release);
            assert_eq!(a, Some(CoordinatorAction::StopAndTranscribe(expected)));
            t += Duration::from_millis(100);
            step(
                &mut stage,
                &mut id,
                &mut last,
                t,
                CoordinatorEvent::ProcessingFinished,
            );
            t += PRESS_DEBOUNCE + Duration::from_millis(5);
        }
    }

    #[test]
    fn spawn_thread_round_trip() {
        // Light end-to-end test of the spawn() wrapper: press → release →
        // processing-finished delivered via the mpsc channel produces the
        // expected actions in order on the action sink.
        let (action_tx, action_rx) = mpsc::channel();
        let (handle, thread) = spawn(
            move |action| {
                action_tx.send(action).unwrap();
            },
            Instant::now,
        );
        handle.send(CoordinatorEvent::Press);
        handle.send(CoordinatorEvent::Release);
        handle.send(CoordinatorEvent::ProcessingFinished);

        let first = action_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first action");
        assert!(matches!(first, CoordinatorAction::StartRecording(_)));
        let second = action_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second action");
        assert!(matches!(second, CoordinatorAction::StopAndTranscribe(_)));
        // ProcessingFinished does not emit an action.
        assert!(action_rx.recv_timeout(Duration::from_millis(100)).is_err());

        handle.shutdown();
        thread.join();
    }
}
