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
//! Stage transitions (the only legal moves) in hold-to-talk mode:
//!
//! ```text
//!     Idle ─── press ───▶ Recording(id)
//!     Recording(id) ── release ──▶ Processing(id)
//!     Processing(id) ── processing_finished(id) ──▶ Idle
//!     Recording(id) ── cancel ──▶ Idle (no Processing — discard audio)
//! ```
//!
//! In **toggle mode** (set via [`Mode::Toggle`] in [`Options::mode`] — the
//! supervisor reads `VOICEPI_TOGGLE` / config and passes the flag through),
//! the listener does not stop on key-release; instead the next chord press
//! ends the recording. Mirrors the Python toggle path:
//!
//! ```text
//!     Idle ─── press ───▶ Recording(id)
//!     Recording(id) ── release ──▶ (no-op, key still bracketing the recording)
//!     Recording(id) ── press ──▶ Processing(id)
//!     Processing(id) ── processing_finished(id) ──▶ Idle
//! ```
//!
//! Everything else is dropped (and logged at debug level via the host
//! callback — the coordinator itself stays silent so tests can assert
//! behaviour without grepping stdout):
//!
//! * `press` while in [`Stage::Recording`] in hold-to-talk mode: ignored
//!   (key-repeat). `press` while in [`Stage::Processing`]: latched and
//!   re-played as a fresh `StartRecording` once `ProcessingFinished`
//!   arrives, so a user who keeps PTT held across two adjacent utterances
//!   doesn't have to release-then-press again to start the next one.
//! * `release` while in [`Stage::Idle`] / [`Stage::Processing`]: dropped.
//!   This is the **drop-guard** that closes the #254-style hole — a release
//!   that races a processing-finished event cannot wake the recorder.
//! * Stale `processing_finished` for a recording id that no longer matches
//!   the live state is ignored — without that guard a delayed completion
//!   from cycle N could yank a new Recording(M) cycle back to Idle.
//!
//! ## Debounce
//!
//! Press events are debounced by [`PRESS_DEBOUNCE`] (~30 ms by default) so
//! a second press that arrives within the debounce window from the same
//! Idle state is suppressed. This matches the host-side jitter we observe
//! when a key bounces on cheap mechanical keyboards (and the Bluetooth
//! headset double-tap pattern). The window is per-stage: we restart it
//! every time we re-enter Idle so the *next* start is not falsely
//! suppressed.

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
/// arrives after a later recording has started is harmlessly ignored
/// because the ids no longer match (mirrors the `_record_epoch` pattern in
/// `vp_keys.py`). Also threaded through `ProcessingFinished` so a delayed
/// completion from a previous cycle cannot clobber the active recording.
pub type RecordingId = u64;

/// Hold-to-talk vs. toggle mode. The supervisor captures this once at
/// install time from the user's `VOICEPI_TOGGLE` / config and passes it in
/// via [`Options::mode`]; it does not change for the lifetime of the
/// subsystem (matches the Python listener, which also captures it at
/// construction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Hold the PTT chord to record, release to stop. Default — matches the
    /// shipping pynput/evdev behaviour with `VOICEPI_TOGGLE` unset.
    #[default]
    HoldToTalk,
    /// Press once to start, press again to stop. Releases are ignored while
    /// recording (the chord doesn't bracket the utterance).
    Toggle,
}

/// Lifecycle state of a single PTT cycle. Owned by the coordinator thread;
/// no other thread ever reads or writes it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// No PTT in flight. The next press will start a recording.
    #[default]
    Idle,
    /// PTT is held / has been pressed in toggle mode. The next release ends
    /// it. The id is bumped every time we enter Recording so the host can
    /// scope cancels (see [`RecordingId`]).
    Recording(RecordingId),
    /// The release fired and the host is busy transcribing. Press / release
    /// events in this state are deferred or dropped — never acted on. We
    /// leave Processing only when the host sends a matching
    /// [`CoordinatorEvent::ProcessingFinished`] (matching = same id).
    /// The id carried here matches the [`Stage::Recording`] cycle whose
    /// release moved us into Processing.
    Processing(RecordingId),
}

/// Lifecycle events the coordinator accepts on its inbound channel.
/// Producers (the rdev manager thread, and the host when it finishes a
/// transcription) send these via [`CoordinatorHandle::send`].
#[derive(Debug, Clone, Copy)]
pub enum CoordinatorEvent {
    /// The bound PTT chord just completed (rising edge — never key-repeat).
    Press,
    /// The bound PTT chord just broke (falling edge).
    Release,
    /// The host finished transcribing / injecting for the given recording
    /// id. Carries the id so a stale completion (cycle N) delivered after
    /// a new Recording (cycle M > N) has begun is dropped without clearing
    /// the live state. Safe to send from any stage.
    ProcessingFinished(RecordingId),
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
    /// Start a new recording with this generation id. The host should
    /// capture the id and pass it back when reporting cancel /
    /// processing-finished.
    StartRecording(RecordingId),
    /// End the current recording and run the transcription pass. The id
    /// matches the [`Self::StartRecording`] that began it.
    StopAndTranscribe(RecordingId),
    /// Discard the in-flight recording — no transcription, no injection.
    CancelRecording(RecordingId),
}

/// Static configuration for the coordinator. Captured once at spawn time
/// and never mutated; everything that varies per event flows through the
/// channel as [`CoordinatorEvent`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub mode: Mode,
}

/// Public handle to the coordinator thread. Cloneable so multiple producers
/// (the rdev manager, the supervisor) can send events into the same state
/// machine without each holding a separate channel.
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
    /// are no-ops. Returns immediately — the thread is joined separately
    /// via the [`CoordinatorThread`] handle.
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
    /// Wait for the coordinator thread to finish (after sending Shutdown
    /// via [`CoordinatorHandle::shutdown`]). Idempotent — safe to call
    /// twice; the second call is a no-op.
    pub fn join(mut self) {
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn the coordinator thread. `action_sink` is invoked on the
/// coordinator thread every time a stage transition produces an action; it
/// MUST be cheap and non-blocking (push onto a channel / spawn a worker),
/// or the next event will be delayed. Returns a [`CoordinatorHandle`] for
/// producers and a [`CoordinatorThread`] for the supervisor's lifecycle.
///
/// `clock` is injected so tests can drive debounce deterministically. In
/// production this is [`Instant::now`].
pub fn spawn<F, C>(
    options: Options,
    action_sink: F,
    clock: C,
) -> (CoordinatorHandle, CoordinatorThread)
where
    F: FnMut(CoordinatorAction) + Send + 'static,
    C: FnMut() -> Instant + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name("vp-hotkey-coordinator".to_owned())
        .spawn(move || coordinator_loop(options, rx, action_sink, clock))
        .expect("hotkey coordinator thread spawn");
    (
        CoordinatorHandle { tx },
        CoordinatorThread { join: Some(join) },
    )
}

/// Per-call mutable state for [`step`]. Bundled so adding a new field
/// doesn't churn every test call site (this struct grew when toggle-mode
/// and the held-press-across-Processing latch landed).
#[derive(Debug, Default)]
pub(super) struct StepState {
    pub stage: Stage,
    pub next_id: RecordingId,
    pub last_idle_press: Option<Instant>,
    /// True when a fresh `Press` arrived while we were in Processing — set
    /// so the next `ProcessingFinished` (which lands when transcription
    /// completes) can immediately start a new recording instead of dropping
    /// the still-held key. Cleared on `Release` (key was let go during
    /// Processing) and on every Idle entry.
    pub pending_press: bool,
}

impl StepState {
    pub(super) fn new() -> Self {
        Self::default()
    }
}

/// Synchronous step function — the heart of the state machine. Exposed
/// `pub(super)` so unit tests can drive it without spinning up a thread;
/// the production [`coordinator_loop`] is a thin wrapper that pumps mpsc
/// events through this same function.
pub(super) fn step(
    state: &mut StepState,
    options: Options,
    now: Instant,
    event: CoordinatorEvent,
) -> Option<CoordinatorAction> {
    match (event, state.stage) {
        (CoordinatorEvent::Press, Stage::Idle) => {
            // Debounce: drop a press that arrives within PRESS_DEBOUNCE of
            // the previous Idle-press. Mostly catches bouncing keyboards
            // and the BT-headset double-tap pattern.
            if let Some(prev) = state.last_idle_press {
                if now.duration_since(prev) < PRESS_DEBOUNCE {
                    return None;
                }
            }
            start_recording(state, now)
        }
        (CoordinatorEvent::Press, Stage::Recording(id)) => {
            if matches!(options.mode, Mode::Toggle) {
                // Toggle mode: second chord press ends the recording.
                state.stage = Stage::Processing(id);
                Some(CoordinatorAction::StopAndTranscribe(id))
            } else {
                // Hold-to-talk: almost always key-repeat. The rising-edge
                // latch in the tracker already filters real repeats, but
                // we belt-and-brace here too.
                None
            }
        }
        (CoordinatorEvent::Press, Stage::Processing(_)) => {
            // The user kept PTT held / pressed again before the previous
            // transcription finished. Latch the press so when we re-enter
            // Idle we can start the next recording without waiting for the
            // user to release-then-press again (P2 #8). No action right
            // now — Processing must complete first.
            state.pending_press = true;
            None
        }
        (CoordinatorEvent::Release, Stage::Recording(id)) => {
            if matches!(options.mode, Mode::Toggle) {
                // Toggle mode: releases do NOT stop a recording. The next
                // chord press is what ends it (P2 #4).
                None
            } else {
                state.stage = Stage::Processing(id);
                Some(CoordinatorAction::StopAndTranscribe(id))
            }
        }
        (CoordinatorEvent::Release, Stage::Processing(_)) => {
            // User let go of the key during Processing. Clear any pending
            // press so we don't auto-restart on Processing completion —
            // the press they made earlier is no longer held.
            state.pending_press = false;
            None
        }
        (CoordinatorEvent::Release, Stage::Idle) => {
            // Drop-guard. A release that arrives in Idle (no recording to
            // end) is the #254-class hole — silently drop it.
            None
        }
        (CoordinatorEvent::Cancel, Stage::Recording(id)) => {
            state.stage = Stage::Idle;
            state.last_idle_press = None; // re-arm debounce so the next press is fresh
            state.pending_press = false;
            Some(CoordinatorAction::CancelRecording(id))
        }
        (CoordinatorEvent::Cancel, _) => {
            // Nothing to cancel in Idle / Processing.
            None
        }
        (CoordinatorEvent::ProcessingFinished(done_id), Stage::Processing(active_id)) => {
            if done_id != active_id {
                // Stale completion (e.g. host re-emitted an old id). The
                // live Processing is still in flight; ignore.
                return None;
            }
            state.stage = Stage::Idle;
            state.last_idle_press = None; // re-arm debounce — the new cycle is fresh
                                          // If the user kept PTT held across Processing, re-fire StartRecording
                                          // immediately (P2 #8). Debounce is intentionally skipped here:
                                          // the press we're acting on is the SAME held key, not a fresh
                                          // chord, so the bouncing-key window doesn't apply.
            if state.pending_press {
                state.pending_press = false;
                return start_recording(state, now);
            }
            None
        }
        (CoordinatorEvent::ProcessingFinished(_), Stage::Idle) => {
            // Host re-emitted a completion after we've already returned to
            // Idle. Harmless no-op.
            None
        }
        (CoordinatorEvent::ProcessingFinished(_), Stage::Recording(_)) => {
            // Stale completion arriving AFTER a new Recording has begun.
            // Dropping it without state change preserves the live
            // recording — without this guard the recording would be
            // silently abandoned with no matching stop (P2 #9).
            None
        }
        (CoordinatorEvent::Shutdown, _) => None,
    }
}

fn start_recording(state: &mut StepState, now: Instant) -> Option<CoordinatorAction> {
    state.last_idle_press = Some(now);
    state.next_id = state.next_id.wrapping_add(1);
    let id = state.next_id;
    state.stage = Stage::Recording(id);
    Some(CoordinatorAction::StartRecording(id))
}

fn coordinator_loop<F, C>(
    options: Options,
    rx: Receiver<CoordinatorEvent>,
    mut action_sink: F,
    mut clock: C,
) where
    F: FnMut(CoordinatorAction),
    C: FnMut() -> Instant,
{
    let mut state = StepState::new();
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
        if let Some(action) = step(&mut state, options, now, event) {
            action_sink(action);
        }
    }
}

#[cfg(test)]
mod tests;
