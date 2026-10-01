//! Coordinator decisions, counters and bounded capture event consumption.

use super::events::CapturedChord;
use super::{format_json, format_plain, CaptureEvent};
use crate::hotkey::coordinator::{
    CoordinatorAction, CoordinatorEvent, CoordinatorHandle, RecordingId,
};
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// Whether an early-exit condition was requested; the [`CaptureEvent::ExitOnChord`]
/// terminal event should be emitted on the first chord match if so.
///
/// Kept as a bool + a decision helper (rather than an enum) so future flags
/// (e.g. `--exit-after-N`) can layer in without rewiring the enum.
pub(super) fn decide_terminal(
    action: &CoordinatorAction,
    exit_on_chord: bool,
    counters: &Counters,
    start: Instant,
) -> Option<CaptureEvent> {
    if !exit_on_chord {
        return None;
    }
    if matches!(action, CoordinatorAction::StartRecording(_)) {
        return Some(CaptureEvent::ExitOnChord {
            t_secs: start.elapsed().as_secs_f64(),
            events: counters.events.load(Ordering::Relaxed),
            chords: counters.chords.load(Ordering::Relaxed),
            foreign_keys: counters.foreign_keys.load(Ordering::Relaxed),
        });
    }
    None
}

/// Shared, thread-safe counters incremented from the rdev listener thread
/// (raw tap) and the coordinator thread (action sink). Read on the main
/// thread when the terminal event fires.
#[derive(Default)]
pub(super) struct Counters {
    pub(super) events: AtomicU64,
    pub(super) chords: AtomicU64,
    pub(super) foreign_keys: AtomicU64,
}

/// Emit `event` on stdout with the requested format, flushing so the
/// buffered writer doesn't hold events past a Ctrl-C.
pub(super) fn emit(event: &CaptureEvent, json: bool, stdout: &mut io::StdoutLock<'_>) {
    let line = if json {
        format_json(event)
    } else {
        format_plain(event)
    };
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

pub(super) type CaptureFocusSnapshot = Arc<dyn Fn() -> Option<bool> + Send + Sync + 'static>;

pub(super) fn focus_snapshot_for_process(target_pid: Option<u32>) -> CaptureFocusSnapshot {
    Arc::new(move || {
        let target_pid = target_pid?;
        crate::platform::foreground_window::foreground_process_id()
            .map(|foreground_pid| foreground_pid == target_pid)
    })
}

pub(super) fn build_capture_action_sink(
    counters: Arc<Counters>,
    event_tx: Sender<CaptureEvent>,
    coord_slot: Arc<OnceLock<CoordinatorHandle>>,
    start: Instant,
    exit_on_chord: bool,
) -> impl FnMut(CoordinatorAction, Option<bool>) + Send + 'static {
    move |action: CoordinatorAction, focused: Option<bool>| {
        if counts_as_chord(&action) {
            counters.chords.fetch_add(1, Ordering::Relaxed);
        }
        let now = start.elapsed().as_secs_f64();
        let event = match action {
            CoordinatorAction::StartRecording(id) => CaptureEvent::ChordMatched {
                t_secs: now,
                id,
                focused,
            },
            CoordinatorAction::StopAndTranscribe(id) => {
                complete_processing_stage(coord_slot.get(), id);
                CaptureEvent::ChordReleased {
                    t_secs: now,
                    id,
                    focused,
                }
            }
            CoordinatorAction::CancelRecording(id) => CaptureEvent::ChordCanceled {
                t_secs: now,
                id,
                focused,
            },
        };
        let _ = event_tx.send(event);
        if let Some(terminal) = decide_terminal(&action, exit_on_chord, &counters, start) {
            let _ = event_tx.send(terminal);
        }
    }
}

pub(super) struct CaptureLoopOptions {
    pub(super) deadline: Instant,
    pub(super) start: Instant,
    pub(super) json: bool,
    pub(super) configure: bool,
}

pub(super) fn capture_until_deadline(
    options: CaptureLoopOptions,
    counters: &Counters,
    event_rx: &mpsc::Receiver<CaptureEvent>,
    captured: &mut CapturedChord,
    stdout: &mut io::StdoutLock<'_>,
) -> Option<String> {
    let mut captured_chord = None;
    loop {
        let now = Instant::now();
        if now >= options.deadline {
            let terminal = CaptureEvent::DurationReached {
                t_secs: options.start.elapsed().as_secs_f64(),
                events: counters.events.load(Ordering::Relaxed),
                chords: counters.chords.load(Ordering::Relaxed),
                foreign_keys: counters.foreign_keys.load(Ordering::Relaxed),
            };
            emit(&terminal, options.json, stdout);
            break;
        }
        let remaining = options.deadline.saturating_duration_since(now);
        match event_rx.recv_timeout(remaining) {
            Ok(event) => {
                emit(&event, options.json, stdout);
                if options.configure {
                    if let Some(chord) = captured.observe(&event) {
                        captured_chord = Some(chord);
                        break;
                    }
                }
                if event.is_terminal() {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    captured_chord
}

// `driver_name` was previously a hard-coded `"rdev"` because the CLI only
// wired up that backend. The evdev listener (audit item 5 prereq 2) now
// makes the choice a runtime decision — read via `HotkeyHandle::driver_name()`
// right after `install_hotkey_with_focus_snapshot` returns instead.

/// Should this coordinator action increment the `Chords:` counter?
///
/// The coordinator emits three actions per chord (`StartRecording` on the
/// rising edge, then `StopAndTranscribe` or `CancelRecording` on release), so
/// counting every action double-reports — a two-chord capture showed
/// `"chords":4`. Only the rising edge counts as one chord fire.
///
/// Extracted from the `action_sink` closure so the producing side of the fix
/// can be unit-tested (`chords_counter_*`) directly. Otherwise only the
/// formatter side of the counter was covered — the same failure mode this
/// PR set out to fix for `foreign_keys`.
pub(super) fn counts_as_chord(action: &CoordinatorAction) -> bool {
    matches!(action, CoordinatorAction::StartRecording(_))
}

/// Build the raw-event tap the manager thread invokes for every OS key
/// event. Isolated into its own helper so the closure has a well-defined
/// capture set — makes the borrow-checker happy and keeps run_capture
/// readable.
/// Hand the coordinator back to Idle after a stop.
///
/// `Release` moves the coordinator to `Stage::Processing(id)`, which it
/// leaves ONLY on a matching `ProcessingFinished`. The shipping runtime sends
/// that when transcription completes; this diagnostic has no session, so
/// completion is immediate and unconditional.
///
/// A `None` handle is a no-op rather than an error: the slot is populated
/// right after install, and the only window where it is empty is before any
/// chord can have fired.
pub(super) fn complete_processing_stage(handle: Option<&CoordinatorHandle>, id: RecordingId) {
    if let Some(handle) = handle {
        handle.send(CoordinatorEvent::ProcessingFinished(id));
    }
}

#[cfg(test)]
#[path = "actions_tests.rs"]
mod tests;
