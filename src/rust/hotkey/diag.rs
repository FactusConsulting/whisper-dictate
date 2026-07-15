//! Diagnostic instrumentation for the hotkey subsystem — added to investigate
//! the Windows PTT wedge that survived PR #476 (`InjectionGuard`).
//!
//! # Background
//!
//! Users on Windows 11 report that PTT works once, then goes silent — even
//! with the injection guard from PR #476 in place. The tell in the log is
//! **complete absence** of any hotkey / worker activity for the second PTT
//! attempt: no "chord rejected" line, no "guard dropped event" line, nothing.
//! That leaves two competing hypotheses:
//!
//! * **(A) The OS unhooked us.** Windows silently disables a
//!   `WH_KEYBOARD_LL` hook whose `LowLevelKeyboardProc` returns slower than
//!   `LowLevelHooksTimeout` (default 300 ms). If a callback pass runs long
//!   (mutex contention, sink chain latency, ...) Windows can drop the hook
//!   and no further events reach the callback. Log evidence: raw-event
//!   counter stays flat across the wedge.
//! * **(B) The tracker silently rejects the chord.** The hook is still
//!   alive, events arrive, but the tracker decides not to emit a
//!   `ChordPress` (rule 1, stale foreign-key entry, already-latched, ...).
//!   Log evidence: raw-event counter advances but no `ChordPress` line.
//!
//! # What this module exposes
//!
//! * A single [`Counters`] block — cheap atomic counters incremented from
//!   the hot path (rdev callback / dispatch / tracker). Aggregated across
//!   the whole subsystem so the heartbeat can report subsystem-wide totals.
//! * A **heartbeat** background thread ([`spawn_heartbeat_once`]) that
//!   prints `[hotkey-diag] heartbeat …` to stderr every
//!   [`HEARTBEAT_INTERVAL`] with the current counters AND the delta since
//!   the previous tick. Runs unconditionally — a heartbeat every 30 s is
//!   negligible log noise and this is exactly the signal that distinguishes
//!   (A) from (B) in a user-supplied log.
//! * An env-gated verbose flag ([`debug_enabled`]) driven by
//!   `VOICEPI_HOTKEY_DEBUG=1`. The rdev callback / injection guard / tracker
//!   consult this to decide whether to emit per-event `[hotkey-diag]` lines
//!   (event kind, guard state, rejection reason, ...). Off by default so
//!   normal builds stay quiet; users who want the deep trace flip the env
//!   var and reproduce.
//!
//! Nothing here changes the tracker / guard **behaviour** — it only reports
//! what already happens. Removing this module (and its call sites) is a
//! pure revert; every branch that logs still returns the same
//! `Option<TrackerOutput>` it did before.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use super::inject_guard::{dispatch_verbose, DispatchOutcome, InjectionGuard};
use super::manager::tracker::{KeyTracker, RawKeyEvent, TrackerDecision, TrackerOutput};

/// Interval between `[hotkey-diag] heartbeat …` stderr lines. 30 s is
/// small enough that a wedge is bracketed by at most two heartbeats
/// (before + after) and large enough that a happy session produces about
/// two lines per minute — inside the noise floor of the app's existing
/// `[worker-rust]` / `[worker]` output.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Hot-path counters for the diagnostic heartbeat. All fields are
/// [`AtomicU64`] so the rdev callback / dispatch / tracker can bump them
/// with a single `fetch_add(1, Relaxed)` — no mutex, no allocation, no
/// risk of blowing the `LowLevelHooksTimeout` budget on Windows.
#[derive(Debug, Default)]
pub struct Counters {
    /// Total raw OS key events observed by the rdev callback (both
    /// keyboard KeyPress and KeyRelease). A flat counter across a wedge
    /// is strong evidence for hypothesis (A): the OS dropped the hook.
    pub raw_events: AtomicU64,
    /// Events dropped by [`crate::hotkey::inject_guard::dispatch_raw_event`]
    /// because the guard was still armed. Advances during the paste /
    /// typing burst; stays flat otherwise.
    pub guard_drops: AtomicU64,
    /// Presses of a target key that did NOT produce `ChordPress` — the
    /// tracker rejected it (key-repeat, chord not yet complete, rule 1
    /// foreign-key hold, already-latched).
    pub tracker_target_rejects: AtomicU64,
    /// `ChordPress` emissions. Every successful PTT start bumps this.
    pub chord_press: AtomicU64,
    /// `ChordRelease` emissions.
    pub chord_release: AtomicU64,
    /// `ChordCancel` emissions (rule 2 foreign key during recording).
    pub chord_cancel: AtomicU64,
}

/// Snapshot of the [`Counters`] taken by the heartbeat thread so it can
/// report deltas without holding a reference across the sleep.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub raw_events: u64,
    pub guard_drops: u64,
    pub tracker_target_rejects: u64,
    pub chord_press: u64,
    pub chord_release: u64,
    pub chord_cancel: u64,
}

impl Counters {
    /// Atomic-load every field into a [`Snapshot`]. Uses `Relaxed`
    /// ordering — this is diagnostic output, not synchronisation.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            raw_events: self.raw_events.load(Ordering::Relaxed),
            guard_drops: self.guard_drops.load(Ordering::Relaxed),
            tracker_target_rejects: self.tracker_target_rejects.load(Ordering::Relaxed),
            chord_press: self.chord_press.load(Ordering::Relaxed),
            chord_release: self.chord_release.load(Ordering::Relaxed),
            chord_cancel: self.chord_cancel.load(Ordering::Relaxed),
        }
    }
}

/// Fetch the process-wide [`Counters`] handle, initialising it on first
/// call. Every call site (rdev callback, dispatch, tracker, heartbeat
/// thread) uses this to bump / read the same counters — a single
/// `OnceLock<Arc<_>>` matches the shape of the process-global slots
/// already used by [`crate::hotkey::inject_guard::global`] and
/// [`crate::runtime::rust_session_sink`].
pub fn counters() -> Arc<Counters> {
    static SLOT: OnceLock<Arc<Counters>> = OnceLock::new();
    Arc::clone(SLOT.get_or_init(|| Arc::new(Counters::default())))
}

/// True iff the caller should emit per-event `[hotkey-diag]` lines. Read
/// from `VOICEPI_HOTKEY_DEBUG=1` (any other value / unset = false) and
/// cached so the hot path is a single atomic load. Change requires an
/// app restart — matches the other `VOICEPI_*` env vars.
pub fn debug_enabled() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("VOICEPI_HOTKEY_DEBUG")
            .map(|v| v.trim() == "1")
            .unwrap_or(false)
    })
}

/// Spawn the heartbeat thread if one has not been spawned yet. Idempotent
/// (a repeat call is a no-op). Called from the rdev / evdev driver
/// `spawn` entry points; tests do not call this so their own binaries
/// stay quiet.
pub fn spawn_heartbeat_once() {
    static SPAWNED: OnceLock<()> = OnceLock::new();
    SPAWNED.get_or_init(|| {
        eprintln!(
            "[hotkey-diag] heartbeat installed (interval={:?}, VOICEPI_HOTKEY_DEBUG={})",
            HEARTBEAT_INTERVAL,
            if debug_enabled() { "on" } else { "off" },
        );
        thread::Builder::new()
            .name("vp-hotkey-diag-heartbeat".to_owned())
            .spawn(heartbeat_loop)
            .expect("hotkey diag heartbeat thread spawn");
    });
}

/// Body of the heartbeat thread — sleeps [`HEARTBEAT_INTERVAL`] between
/// stderr lines, reporting the running totals plus the per-interval delta.
/// A flat `raw` delta while the user is pressing keys is the smoking-gun
/// signature of hypothesis (A) (OS unhooked us). A rising `raw` delta with
/// zero `chord_press` and rising `tracker_rejects` is hypothesis (B).
fn heartbeat_loop() {
    let counters = counters();
    let mut prev = counters.snapshot();
    loop {
        thread::sleep(HEARTBEAT_INTERVAL);
        let cur = counters.snapshot();
        eprintln!(
            "[hotkey-diag] heartbeat raw={} (+{}) guard_drops={} (+{}) tracker_rejects={} (+{}) chord_press={} (+{}) chord_release={} (+{}) chord_cancel={} (+{})",
            cur.raw_events,
            cur.raw_events.saturating_sub(prev.raw_events),
            cur.guard_drops,
            cur.guard_drops.saturating_sub(prev.guard_drops),
            cur.tracker_target_rejects,
            cur.tracker_target_rejects
                .saturating_sub(prev.tracker_target_rejects),
            cur.chord_press,
            cur.chord_press.saturating_sub(prev.chord_press),
            cur.chord_release,
            cur.chord_release.saturating_sub(prev.chord_release),
            cur.chord_cancel,
            cur.chord_cancel.saturating_sub(prev.chord_cancel),
        );
        prev = cur;
    }
}

/// End-to-end event handler for the rdev callback — takes one raw event,
/// snapshots the tracker + guard around dispatch, updates diagnostic
/// counters, and emits the always-on `[hotkey-diag] evt#…` trace line.
/// Extracted from the rdev driver so the callback closure stays a
/// one-liner and the callback file stays under the repo-wide 500-LOC
/// per-file rule.
///
/// The `sink` reference is invoked (with the mutex released) for any
/// `TrackerOutput` the tracker produced. Every log line documents the
/// state transition the event caused (`held_before` / `held_after` /
/// `latched` / `emitted` / decision label + payload) so a wedge trace
/// pastes into a bug report and is directly readable.
pub fn handle_raw_event_with_diag(
    raw: &RawKeyEvent,
    tracker: &Mutex<KeyTracker>,
    guard: &InjectionGuard,
    counters: &Counters,
    sink: &(dyn Fn(TrackerOutput) + Send + Sync),
) {
    // Bump the raw-event counter BEFORE anything else so hypothesis-A
    // evidence (OS unhooked us) shows up in the heartbeat even if the
    // mutex acquire below would hang. Relaxed ordering: this is
    // diagnostic, and the heartbeat thread only reads for stderr output.
    let evt_no = counters.raw_events.fetch_add(1, Ordering::Relaxed) + 1;

    // Guard state at event arrival — snapshotted before we lock the
    // tracker mutex so a wedge scenario "guard was still armed" or
    // "guard expired N ms ago" is visible per event.
    let event_at_ms = raw.at.saturating_duration_since(guard.epoch()).as_millis() as i64;
    let active_until = guard.active_until_ms() as i64;
    let guard_delta_ms = active_until - event_at_ms;

    let mut t = tracker.lock().expect("tracker poisoned");
    let held_before = t.held_snapshot();
    let latched_before = t.is_chord_latched();
    let emitted_before = t.is_chord_emitted();

    // `dispatch_verbose` returns Dispatched / DroppedByGuard so the diag
    // log distinguishes "guard swallowed this" from "tracker classified
    // it as X". Behaviour is byte-identical to `dispatch_raw_event`.
    let dispatch = dispatch_verbose(guard, &mut t, raw);
    let (outcome_label, decision_detail): (&str, String) = match &dispatch {
        DispatchOutcome::DroppedByGuard => {
            counters.guard_drops.fetch_add(1, Ordering::Relaxed);
            ("dropped_by_guard", String::new())
        }
        DispatchOutcome::Dispatched(decision) => {
            bump_decision_counter(counters, decision);
            (decision.label(), format!("{decision:?}"))
        }
    };

    // Snapshot the tracker AFTER dispatch so `held_before`/`held_after`
    // shows the state transition each event caused.
    let held_after = t.held_snapshot();
    let latched_after = t.is_chord_latched();
    let emitted_after = t.is_chord_emitted();

    // Only invoke the coordinator sink once the state is captured. Drop
    // the mutex before running the sink so a downstream slow path never
    // blocks the next OS event.
    let output = match &dispatch {
        DispatchOutcome::Dispatched(d) => d.output(),
        _ => None,
    };
    drop(t);

    // Always-on per-event trace. One line per event so grep is easy;
    // the state deltas around each event make hypothesis-B corruption
    // paths visible directly ("held_before=[]" pre-injection then
    // "held_after=[__rdev_KeyH]" post-injection shows a synthetic press
    // leaked past the guard into the pressed map).
    eprintln!(
        "[hotkey-diag] evt#{evt_no} t={event_at_ms}ms name={name:?} kind={kind:?} \
         guard=until{sign}{delta}ms ({guard_state}) \
         held_before={held_before:?} latched={latched_before} emitted={emitted_before} \
         -> {outcome_label} \
         held_after={held_after:?} latched={latched_after} emitted={emitted_after} \
         detail={decision_detail}",
        name = raw.name,
        kind = raw.kind,
        sign = if guard_delta_ms >= 0 { "+" } else { "" },
        delta = guard_delta_ms,
        guard_state = if guard_delta_ms > 0 {
            "armed"
        } else {
            "inactive"
        },
    );

    if let Some(out) = output {
        sink(out);
    }
}

/// Bump the specific counter for a [`TrackerDecision`]. Pulled out of
/// [`handle_raw_event_with_diag`] so the branch shape stays flat and the
/// hot-path bookkeeping is contained. `is_target_reject` intentionally
/// EXCLUDES `NonTargetPress` / `NonTargetRelease` — ordinary typing
/// (every character the user hits between PTT chords) hits those and
/// would otherwise inflate `tracker_target_rejects` beyond usefulness.
fn bump_decision_counter(counters: &Counters, decision: &TrackerDecision) {
    match decision {
        TrackerDecision::ChordPress => {
            counters.chord_press.fetch_add(1, Ordering::Relaxed);
        }
        TrackerDecision::ChordRelease => {
            counters.chord_release.fetch_add(1, Ordering::Relaxed);
        }
        TrackerDecision::ChordCancel => {
            counters.chord_cancel.fetch_add(1, Ordering::Relaxed);
        }
        d if d.is_target_reject() => {
            counters
                .tracker_target_rejects
                .fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_process_wide_single_instance() {
        // The whole subsystem shares one `Arc<Counters>`; verify that
        // two calls to `counters()` return handles that point at the
        // same underlying atomics (bump via one, observe via the other).
        // Snapshot baselines first — other tests in the same binary may
        // have bumped these counters already. We only care that a bump
        // through `a` is visible through `b`, not that the total starts
        // at zero.
        let a = counters();
        let b = counters();
        let before = b.raw_events.load(Ordering::Relaxed);
        a.raw_events.fetch_add(1, Ordering::Relaxed);
        assert_eq!(b.raw_events.load(Ordering::Relaxed), before + 1);
    }

    #[test]
    fn snapshot_reflects_current_counter_values() {
        let ctr = counters();
        let before = ctr.snapshot();
        ctr.chord_cancel.fetch_add(3, Ordering::Relaxed);
        let after = ctr.snapshot();
        assert_eq!(after.chord_cancel, before.chord_cancel + 3);
    }

    use crate::hotkey::manager::tracker::RawKeyKind;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn handle_raw_event_dispatches_and_bumps_counters() {
        // End-to-end contract: a real user chord press goes through the
        // guard (unarmed here), lands in the tracker as ChordPress, and
        // the ChordPress counter and raw_events counter both advance.
        // Sink is invoked with the emitted output.
        let tracker = StdMutex::new(KeyTracker::new(vec!["ctrl_l".to_owned()]));
        let guard = InjectionGuard::new();
        // Use a locally-constructed Counters — we're testing the free
        // function; the process-global slot is exercised by
        // `counters_are_process_wide_single_instance`.
        let counters = Counters::default();
        let sink_hits = Arc::new(AtomicU64::new(0));
        let sink_hits_cb = Arc::clone(&sink_hits);
        let sink = move |_out: TrackerOutput| {
            sink_hits_cb.fetch_add(1, Ordering::Relaxed);
        };
        let raw = RawKeyEvent {
            name: "ctrl_l".to_owned(),
            kind: RawKeyKind::Press,
            at: std::time::Instant::now(),
        };
        handle_raw_event_with_diag(&raw, &tracker, &guard, &counters, &sink);
        assert_eq!(counters.raw_events.load(Ordering::Relaxed), 1);
        assert_eq!(counters.chord_press.load(Ordering::Relaxed), 1);
        assert_eq!(counters.guard_drops.load(Ordering::Relaxed), 0);
        assert_eq!(sink_hits.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn handle_raw_event_bumps_guard_drops_when_guard_armed() {
        // Guard armed → event never reaches tracker, guard_drops++, sink
        // is NOT invoked. This is the primary behaviour we want the
        // diagnostic to make visible in the wedge trace.
        let tracker = StdMutex::new(KeyTracker::new(vec!["ctrl_l".to_owned()]));
        let guard = InjectionGuard::new();
        guard.arm(Duration::from_millis(200));
        let counters = Counters::default();
        let sink_hits = Arc::new(AtomicU64::new(0));
        let sink_hits_cb = Arc::clone(&sink_hits);
        let sink = move |_out: TrackerOutput| {
            sink_hits_cb.fetch_add(1, Ordering::Relaxed);
        };
        let raw = RawKeyEvent {
            name: "ctrl_l".to_owned(),
            kind: RawKeyKind::Press,
            at: std::time::Instant::now(),
        };
        handle_raw_event_with_diag(&raw, &tracker, &guard, &counters, &sink);
        assert_eq!(counters.raw_events.load(Ordering::Relaxed), 1);
        assert_eq!(counters.guard_drops.load(Ordering::Relaxed), 1);
        assert_eq!(counters.chord_press.load(Ordering::Relaxed), 0);
        assert_eq!(sink_hits.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn handle_raw_event_bumps_target_rejects_on_rule1_block() {
        // Foreign key held before target → tracker rejects the target
        // press with TargetBlockedByForeignKey. Counter for target
        // rejects advances; ChordPress does NOT. This is exactly the
        // signal that would appear in the log for a hypothesis-B wedge.
        let tracker = StdMutex::new(KeyTracker::new(vec!["ctrl_l".to_owned()]));
        let guard = InjectionGuard::new();
        let counters = Counters::default();
        let sink = |_out: TrackerOutput| {};
        // Foreign press first — no target reject (it's a non-target).
        handle_raw_event_with_diag(
            &RawKeyEvent {
                name: "a".to_owned(),
                kind: RawKeyKind::Press,
                at: std::time::Instant::now(),
            },
            &tracker,
            &guard,
            &counters,
            &sink,
        );
        assert_eq!(counters.tracker_target_rejects.load(Ordering::Relaxed), 0);
        // Now the target press: rule 1 blocks, counter bumps.
        handle_raw_event_with_diag(
            &RawKeyEvent {
                name: "ctrl_l".to_owned(),
                kind: RawKeyKind::Press,
                at: std::time::Instant::now(),
            },
            &tracker,
            &guard,
            &counters,
            &sink,
        );
        assert_eq!(counters.tracker_target_rejects.load(Ordering::Relaxed), 1);
        assert_eq!(counters.chord_press.load(Ordering::Relaxed), 0);
    }
}
