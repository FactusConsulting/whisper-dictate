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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
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
    /// FIFO barrier used by runtime stop/restart. When this event is
    /// acknowledged, every action queued before it—including a synchronous
    /// transcription/injection pass—has returned.
    Barrier(u64),
    /// Stop the coordinator thread cleanly. Sent by
    /// [`CoordinatorHandle::shutdown`].
    Shutdown,
}

/// Side-effects the coordinator asks the host to perform. The action sink is
/// invoked synchronously, which gives stop/restart's FIFO barrier a precise
/// guarantee that an older transcription/injection action has returned.
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
    /// When true, [`coordinator_loop`] synthesises a matching
    /// [`CoordinatorEvent::ProcessingFinished`] IMMEDIATELY after emitting
    /// [`CoordinatorAction::StopAndTranscribe`] — before it reads the next
    /// event from its inbound queue. Callers that have no real transcription
    /// pass to wait for (the `hotkey capture` diagnostic) want this so a
    /// press/release pair already queued behind the release doesn't land in
    /// [`Stage::Processing`], where the release would silently clear
    /// `pending_press` and the second chord would be swallowed (P2 review of
    /// #612: "complete processing before consuming the next chord").
    ///
    /// The shipping runtime leaves this at the default `false` — real
    /// transcription DOES take time, and only the host knows when it is
    /// finished, so the completion must come from the host.
    pub auto_complete_processing: bool,
}

/// Public handle to the coordinator thread. Cloneable so multiple producers
/// (the rdev manager, the supervisor) can send events into the same state
/// machine without each holding a separate channel.
#[derive(Clone)]
pub struct CoordinatorHandle {
    tx: Sender<CoordinatorEvent>,
    next_barrier: Arc<AtomicU64>,
    completed_barrier: Arc<(Mutex<u64>, Condvar)>,
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

    /// Wait until the coordinator has completed every event already queued.
    ///
    /// Stop/restart uses this after `Cancel` so it cannot reopen injection
    /// while an older synchronous StopAndTranscribe action is still running.
    pub fn quiesce(&self) -> Result<(), String> {
        let id = self.next_barrier.fetch_add(1, Ordering::Relaxed) + 1;
        self.tx
            .send(CoordinatorEvent::Barrier(id))
            .map_err(|_| "hotkey coordinator stopped before quiesce barrier".to_owned())?;
        let (completed, wake) = &*self.completed_barrier;
        let mut completed = completed
            .lock()
            .map_err(|_| "hotkey coordinator quiesce lock poisoned".to_owned())?;
        while *completed < id {
            completed = wake
                .wait(completed)
                .map_err(|_| "hotkey coordinator quiesce wait poisoned".to_owned())?;
        }
        Ok(())
    }

    /// Build a disconnected handle — the paired receiver is dropped, so
    /// every [`Self::send`] silently no-ops. Exists solely so the stock
    /// (no `rust-hotkeys` feature) [`super::HotkeyHandle`] can satisfy the
    /// `coordinator_handle()` accessor's return type; that stub install
    /// path never returns a live `HotkeyHandle`, so the handle produced
    /// here is unreachable at runtime. Gated to the stock build so the
    /// feature build doesn't flag it as dead code.
    #[cfg(not(feature = "rust-hotkeys"))]
    pub(crate) fn disconnected() -> Self {
        let (tx, _rx) = mpsc::channel();
        Self {
            tx,
            next_barrier: Arc::new(AtomicU64::new(0)),
            completed_barrier: Arc::new((Mutex::new(0), Condvar::new())),
        }
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
/// coordinator thread every time a stage transition produces an action.
/// Runtime actions may deliberately block through transcription; queued
/// events remain serialized behind that action. Returns a [`CoordinatorHandle`] for
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
    let next_barrier = Arc::new(AtomicU64::new(0));
    let completed_barrier = Arc::new((Mutex::new(0), Condvar::new()));
    let loop_barrier = Arc::clone(&completed_barrier);
    let join = thread::Builder::new()
        .name("vp-hotkey-coordinator".to_owned())
        .spawn(move || coordinator_loop(options, rx, action_sink, clock, loop_barrier))
        .expect("hotkey coordinator thread spawn");
    (
        CoordinatorHandle {
            tx,
            next_barrier,
            completed_barrier,
        },
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
    let deep = crate::diag::debug_enabled();
    let before = state.stage;
    let out = step_inner(state, options, now, event);
    if deep {
        // Trace EVERY step call so a wedge in Processing (host never
        // sent ProcessingFinished) or an unexpected Cancel-on-Idle is
        // visible against the coordinator's own view. Same-stage entries
        // (e.g. a Press that landed as key-repeat) are logged as
        // `Recording(N)-->Recording(N)` so the reader can still tell the
        // event reached the coordinator at all — that's the whole
        // question the F9 investigation is trying to answer.
        crate::diag::log!(
            "[coord] state {:?} --{:?}--> {:?}; action={:?}",
            before,
            event,
            state.stage,
            out
        );
    }
    out
}

fn step_inner(
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
            // Hold-to-talk: user let go of the PTT key during Processing.
            // Clear the pending latch — the held-press that triggered it
            // is no longer in effect, so we must NOT auto-restart when
            // ProcessingFinished arrives.
            //
            // Toggle mode: a key-up during Processing is the natural
            // follow-through of a quick tap (press #N → stop, release of
            // #N) and must NOT wipe a latch set by that same press. Clearing
            // it here would silently drop the queued start that the user
            // just requested (P2 #346 finding 5).
            if !matches!(options.mode, Mode::Toggle) {
                state.pending_press = false;
            }
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
            // Nothing to cancel in Idle / Processing, but wipe the pending
            // latch so a cancel that arrives while in Processing doesn't
            // trigger a spurious restart when ProcessingFinished fires
            // (P2 #346 finding 3).
            state.pending_press = false;
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
        (CoordinatorEvent::Barrier(_), _) => None,
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
    completed_barrier: Arc<(Mutex<u64>, Condvar)>,
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
        if let CoordinatorEvent::Barrier(id) = event {
            let (completed, wake) = &*completed_barrier;
            if let Ok(mut completed) = completed.lock() {
                *completed = (*completed).max(id);
                wake.notify_all();
            }
            continue;
        }
        let now = clock();
        if let Some(action) = step(&mut state, options, now, event) {
            action_sink(action);
            // Auto-complete-processing is the diagnostic's escape hatch: it
            // has no real transcription to wait for, so leaving the state
            // machine in `Processing` until an out-of-band
            // `ProcessingFinished` lands on the queue lets any press/release
            // pair already queued behind the release be handled in
            // `Processing`. The release there clears `pending_press`, so the
            // second chord is silently swallowed even though nothing was
            // actually processing. Synthesising `ProcessingFinished`
            // synchronously here — BEFORE reading the next event — is what
            // makes the ordering deterministic. Guarded so shipping runtime
            // (`auto_complete_processing == false`) is untouched.
            if options.auto_complete_processing {
                if let CoordinatorAction::StopAndTranscribe(id) = action {
                    let now = clock();
                    if let Some(followup) = step(
                        &mut state,
                        options,
                        now,
                        CoordinatorEvent::ProcessingFinished(id),
                    ) {
                        action_sink(followup);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
