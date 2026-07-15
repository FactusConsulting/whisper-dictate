//! Self-injection guard — filters the OS key events our own text injector
//! synthesises out of the PTT tracker's input stream.
//!
//! ## Why this exists (Windows PTT wedge)
//!
//! On Windows the [`crate::injection::enigo_backend`] injector reaches the
//! OS via `SendInput`. Those synthetic events flow through **every**
//! `WH_KEYBOARD_LL` hook — including the one `rdev` installs for the PTT
//! listener — because rdev 0.5's callback does not inspect
//! `KBDLLHOOKSTRUCT.flags & LLKHF_INJECTED`. The consequence: every
//! character the app types after a transcription feeds back into the PTT
//! tracker, along with the [`crate::dictate::backends::inject::STALE_MODIFIER_VKS`]
//! release sweep (`VK_SHIFT`, `VK_CONTROL`, `VK_LWIN`, …) — some of which
//! rdev DOES resolve to real names (`shift_r`, `ctrl_r`, `alt_gr`,
//! `cmd_l`, …). That stream can leave the tracker's `pressed` map
//! populated with stray foreign keys, tripping bare-modifier rule 1 for
//! the *next* PTT press — which then never fires until the 10 s foreign-
//! key self-heal expires. Symptom the user reports: **"PTT works once,
//! then can't be activated again"**.
//!
//! Same class of bug as #467 on Linux/Wayland, where the fix was to
//! exclude the `ydotoold` virtual `/dev/input` node from the evdev
//! listener's device enumeration (that channel is device-level; Windows
//! has no equivalent). Here we filter at the event-stream layer: the
//! injector *arms* the guard before every `SendInput` burst, and the
//! rdev driver's callback drops every event that arrives while the guard
//! is active.
//!
//! ## Timing model — arm-with-grace, no explicit disarm
//!
//! `WH_KEYBOARD_LL` events reach the hook callback via the installing
//! thread's message pump, which runs on rdev's listener thread. That
//! means `SendInput` on the injecting thread can return **before** the
//! LL hook has drained the queued injected events. A naive
//! begin/end-around-SendInput flag would race the hook's message pump
//! and leak the tail of every burst into the tracker.
//!
//! Instead this guard exposes a single `arm(grace)` primitive that
//! extends the "no events before" horizon forward — never backward. The
//! injector calls it *twice*: once just before the burst (to cover the
//! synthesis itself) and once just after (to cover the LL-hook drain
//! latency). The horizon then decays on its own after the grace period
//! elapses, and any real user press that lands after the grace window
//! is picked up normally. Grace values in production: 50 ms pre-arm +
//! 300 ms post-arm (see `crate::dictate::backends::inject`).
//!
//! ## Testability
//!
//! The guard is a pure `AtomicU64` + `Instant` epoch — no globals, no
//! I/O, no threads — so its arm / expiry semantics are unit-tested
//! directly here. Production wiring plumbs an `Arc<InjectionGuard>` from
//! [`super::install_hotkey`] into both the rdev driver's callback and
//! the injector wrapper; tests can construct their own guard, arm it,
//! and drive the driver's [`dispatch_raw_event`] helper without spawning
//! any OS listener.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::manager::tracker::{KeyTracker, RawKeyEvent, TrackerDecision, TrackerOutput};

/// Process-wide self-injection guard. Cloneable through `Arc` — one
/// instance is created per `install_hotkey` call and shared with both the
/// hotkey driver's callback and the injector wrapper. See the module doc
/// for the timing rationale.
///
/// State is a single monotonically-non-decreasing "no events before" tick
/// count relative to a fixed epoch captured at construction. `arm(grace)`
/// extends the horizon; `is_active()` compares the horizon against
/// `now`. No explicit disarm — the horizon decays on its own so a
/// forgotten disarm cannot wedge the listener forever.
#[derive(Debug)]
pub struct InjectionGuard {
    /// Milliseconds since [`Self::epoch`] before which any observed OS
    /// key event is treated as self-injected. `0` means "never active".
    active_until_millis: AtomicU64,
    /// Monotonic reference point for [`Self::active_until_millis`].
    /// Captured once at construction so `arm` / `is_active` do not
    /// depend on wall-clock time (which can jump backwards).
    epoch: Instant,
}

impl InjectionGuard {
    /// Build a fresh (unarmed) guard. Cheap — no I/O, no allocations
    /// besides the containing `Arc` at the call site.
    pub fn new() -> Self {
        Self {
            active_until_millis: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    /// True iff a self-injection burst is currently in progress OR the
    /// post-burst grace window has not yet elapsed. Called from the
    /// hotkey driver's callback on every OS event to decide whether to
    /// forward the event to the tracker.
    pub fn is_active(&self) -> bool {
        self.is_active_at(Instant::now())
    }

    /// [`Self::is_active`] with an injected `now` — used by tests to
    /// probe the horizon without waiting real wall-clock time.
    pub fn is_active_at(&self, now: Instant) -> bool {
        let now_ms = now.saturating_duration_since(self.epoch).as_millis() as u64;
        self.active_until_millis.load(Ordering::SeqCst) > now_ms
    }

    /// Extend the "no events before" horizon to at least `now + grace`.
    /// Never shortens — a late [`Self::arm`] with a small grace cannot
    /// pull the horizon backwards past an earlier long-grace arm still
    /// in flight (that would let injected tail events leak through).
    pub fn arm(&self, grace: Duration) {
        self.arm_at(Instant::now(), grace);
    }

    /// Time (relative to the guard's epoch) at which the "no events
    /// before" horizon expires. `0` means "never armed". Exposed for the
    /// diagnostic log so the rdev driver can report how far each event
    /// is inside or outside the guard window (a wedge scenario where
    /// injected events land *just outside* a too-short horizon shows up
    /// as a small positive delta between `event_at_ms` and this value).
    pub fn active_until_ms(&self) -> u64 {
        self.active_until_millis.load(Ordering::SeqCst)
    }

    /// The [`Instant`] epoch this guard is measuring against. Used with
    /// [`Self::active_until_ms`] and the incoming event's timestamp to
    /// compute a diff in the diagnostic log.
    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    /// [`Self::arm`] with an injected `now` — used by unit tests.
    pub fn arm_at(&self, now: Instant, grace: Duration) {
        let now_ms = now.saturating_duration_since(self.epoch).as_millis() as u64;
        let new_until = now_ms.saturating_add(grace.as_millis() as u64);
        // Compare-and-swap loop so a concurrent arm from another thread
        // cannot reduce the horizon: we only ever move it forward.
        loop {
            let cur = self.active_until_millis.load(Ordering::SeqCst);
            if cur >= new_until {
                return;
            }
            if self
                .active_until_millis
                .compare_exchange(cur, new_until, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return;
            }
        }
    }
}

impl Default for InjectionGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Route one raw OS key event through `tracker`, filtering it if `guard`
/// says a self-injection burst is in flight. This is the exact predicate
/// the hotkey driver's callback runs — extracted here so tests can drive
/// the guard/tracker interaction end-to-end without spawning a real
/// rdev/evdev listener.
///
/// Thin wrapper over [`dispatch_verbose`] for callers (and tests) that
/// only care about the coordinator-visible side-effect. The rdev driver
/// itself uses [`dispatch_verbose`] so its diagnostic logs can
/// distinguish "guard swallowed this event" from "tracker classified it
/// as X".
pub fn dispatch_raw_event(
    guard: &InjectionGuard,
    tracker: &mut KeyTracker,
    event: &RawKeyEvent,
) -> Option<TrackerOutput> {
    match dispatch_verbose(guard, tracker, event) {
        DispatchOutcome::DroppedByGuard => None,
        DispatchOutcome::Dispatched(decision) => decision.output(),
    }
}

/// Outcome of [`dispatch_verbose`] — either the injection guard swallowed
/// the event, or the tracker reached a [`TrackerDecision`] (which the
/// caller can inspect for the diagnostic reason even when it produced
/// no coordinator-visible output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// The guard was armed when this event arrived — event was dropped
    /// before the tracker saw it. Behaviour-equivalent to the pre-#476
    /// path where the tracker returned `None` for this event.
    DroppedByGuard,
    /// The guard was inactive; the tracker processed the event and
    /// returned this decision. Use [`TrackerDecision::output`] to get
    /// the classic `Option<TrackerOutput>` for the coordinator sink.
    Dispatched(TrackerDecision),
}

/// [`dispatch_raw_event`] with a richer return type so the caller can
/// tell the "guard swallowed the event" arm apart from every specific
/// tracker decision. Semantics are identical to [`dispatch_raw_event`]
/// on the state transition — this only widens the observed reason.
pub fn dispatch_verbose(
    guard: &InjectionGuard,
    tracker: &mut KeyTracker,
    event: &RawKeyEvent,
) -> DispatchOutcome {
    if guard.is_active_at(event.at) {
        return DispatchOutcome::DroppedByGuard;
    }
    DispatchOutcome::Dispatched(tracker.handle_verbose(event))
}

// ------- process-global guard slot -------
//
// The runtime's session (containing the `EnigoInjectBackend`) is built
// BEFORE `install_hotkey` runs — so the injector has no way to obtain
// the guard `Arc` at construction time. Rather than thread an
// `Arc<OnceLock<_>>` through five layers of session/sink wiring, we
// publish the `Arc<InjectionGuard>` created by `install_hotkey` into a
// process-global `OnceLock` and let the injector read it back on each
// `inject()` call. It's genuinely process-scoped state (one hotkey
// subsystem per process, one active injector per session), so the
// global here matches the actual sharing shape.
//
// Tests that want to isolate from the global can either (a) construct
// their own `InjectionGuard` and call [`dispatch_raw_event`] directly
// without touching the global, or (b) install an explicit guard on the
// injector via `EnigoInjectBackend::with_injection_guard` — that
// override takes precedence over [`global`] on the read path.

static GLOBAL_INJECTION_GUARD: OnceLock<Arc<InjectionGuard>> = OnceLock::new();

/// Publish `guard` as the process-wide injection guard. First writer
/// wins; subsequent calls are silently ignored so a test binary that
/// installs the hotkey subsystem twice (e.g. two integration tests in
/// the same process) does not panic. In production `install_hotkey`
/// runs exactly once per process lifetime.
pub fn set_global(guard: Arc<InjectionGuard>) {
    let _ = GLOBAL_INJECTION_GUARD.set(guard);
}

/// Fetch the process-wide injection guard, if `install_hotkey` has
/// populated it. Returns a cheap `Arc` clone. Used by the injector's
/// arm-around-SendInput fallback path (see
/// `crate::dictate::backends::EnigoInjectBackend::inject`).
pub fn global() -> Option<Arc<InjectionGuard>> {
    GLOBAL_INJECTION_GUARD.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::hotkey::manager::tracker::RawKeyKind;

    // ------- InjectionGuard: arm / decay semantics -------

    #[test]
    fn new_guard_is_inactive() {
        let g = InjectionGuard::new();
        assert!(
            !g.is_active(),
            "freshly-constructed guard must not be active"
        );
    }

    #[test]
    fn arm_makes_guard_active_within_grace_and_expires_after() {
        let g = InjectionGuard::new();
        let t0 = g.epoch;
        g.arm_at(t0, Duration::from_millis(100));
        // Well inside the grace window.
        assert!(g.is_active_at(t0 + Duration::from_millis(50)));
        // Just outside the window (strict inequality — the horizon is
        // exclusive so the exact boundary is *not* active).
        assert!(!g.is_active_at(t0 + Duration::from_millis(100)));
        assert!(!g.is_active_at(t0 + Duration::from_millis(200)));
    }

    #[test]
    fn arm_never_shortens_horizon() {
        // Simulates "long pre-arm + short post-arm": the post-arm must not
        // pull the horizon backwards or the LL-hook tail would leak.
        let g = InjectionGuard::new();
        let t0 = g.epoch;
        g.arm_at(t0, Duration::from_millis(500));
        g.arm_at(t0 + Duration::from_millis(50), Duration::from_millis(50));
        // The 500 ms horizon must still hold at t0 + 200 ms — the short
        // arm would have expired but the long one keeps it alive.
        assert!(g.is_active_at(t0 + Duration::from_millis(200)));
    }

    #[test]
    fn arm_extends_horizon_forward() {
        // "Chained" arms cover a longer-than-one-arm burst without gap.
        let g = InjectionGuard::new();
        let t0 = g.epoch;
        g.arm_at(t0, Duration::from_millis(100));
        // Re-arm right before the first horizon expires with a bigger
        // grace — the horizon moves forward.
        g.arm_at(t0 + Duration::from_millis(80), Duration::from_millis(300));
        assert!(g.is_active_at(t0 + Duration::from_millis(200)));
        assert!(g.is_active_at(t0 + Duration::from_millis(350)));
        assert!(!g.is_active_at(t0 + Duration::from_millis(400)));
    }

    // ------- dispatch_raw_event: the regression scenario -------

    fn press_at(name: &str, at: Instant) -> RawKeyEvent {
        RawKeyEvent {
            name: name.to_owned(),
            kind: RawKeyKind::Press,
            at,
        }
    }

    fn release_at(name: &str, at: Instant) -> RawKeyEvent {
        RawKeyEvent {
            name: name.to_owned(),
            kind: RawKeyKind::Release,
            at,
        }
    }

    #[test]
    fn dispatch_drops_events_while_guard_is_active() {
        // Guard armed → dispatch must NOT reach the tracker, so a
        // synthetic self-injected letter press cannot leak into the
        // `pressed` map and trip bare-modifier rule 1 on the next PTT.
        let g = InjectionGuard::new();
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned(), "shift_l".to_owned()]);
        let t0 = g.epoch;
        g.arm_at(t0, Duration::from_millis(200));

        // Simulate the sort of stray event `enigo::text` bursts through
        // WH_KEYBOARD_LL — an unmapped VK the tracker would otherwise
        // treat as a foreign key.
        let injected_press = press_at("__rdev_Unknown(231)", t0 + Duration::from_millis(10));
        assert_eq!(dispatch_raw_event(&g, &mut t, &injected_press), None);
        let injected_release = release_at("__rdev_Unknown(231)", t0 + Duration::from_millis(11));
        assert_eq!(dispatch_raw_event(&g, &mut t, &injected_release), None);

        // Also drop injected modifier releases that DO resolve to real
        // rdev names (e.g. `ctrl_r` from the STALE_MODIFIER_VKS sweep).
        let injected_ctrl_r = release_at("ctrl_r", t0 + Duration::from_millis(12));
        assert_eq!(dispatch_raw_event(&g, &mut t, &injected_ctrl_r), None);
    }

    #[test]
    fn dispatch_forwards_events_after_guard_expires() {
        // The regression scenario end-to-end: injected events during the
        // guard window are dropped; the very next PTT chord press (after
        // the grace expires) STILL fires ChordPress. Without the fix, an
        // injected foreign press would trip rule 1 and this assertion
        // would fail (ChordPress would come back as None).
        let g = InjectionGuard::new();
        let mut tr = KeyTracker::new(vec!["ctrl_l".to_owned(), "shift_l".to_owned()]);
        let t0 = g.epoch;

        // Cycle 1: user's first PTT chord — fires ChordPress + ChordRelease.
        let real_ctrl = press_at("ctrl_l", t0);
        assert_eq!(dispatch_raw_event(&g, &mut tr, &real_ctrl), None);
        let real_shift = press_at("shift_l", t0 + Duration::from_millis(5));
        assert_eq!(
            dispatch_raw_event(&g, &mut tr, &real_shift),
            Some(TrackerOutput::ChordPress)
        );
        let real_shift_up = release_at("shift_l", t0 + Duration::from_millis(200));
        assert_eq!(
            dispatch_raw_event(&g, &mut tr, &real_shift_up),
            Some(TrackerOutput::ChordRelease)
        );
        let real_ctrl_up = release_at("ctrl_l", t0 + Duration::from_millis(210));
        assert_eq!(dispatch_raw_event(&g, &mut tr, &real_ctrl_up), None);

        // Injection begins: guard is armed.
        let t_inject = t0 + Duration::from_millis(500);
        g.arm_at(t_inject, Duration::from_millis(300));

        // A burst of injected foreign keys (letters, stale-modifier
        // releases) flows through. Without the guard, `foreign_press`
        // below would trip bare-modifier rule 1 (rule 1: refuse to
        // start while a foreign key is held) and the *next* PTT chord
        // would silently not fire — the exact wedge the user reports.
        let foreign_press = press_at("__rdev_Unknown(231)", t_inject + Duration::from_millis(20));
        assert_eq!(dispatch_raw_event(&g, &mut tr, &foreign_press), None);

        // Cycle 2: user re-presses PTT AFTER the injection grace window.
        // Must fire ChordPress — no wedge.
        let t_next = t_inject + Duration::from_millis(500);
        assert!(!g.is_active_at(t_next), "guard must have decayed by now");
        let next_ctrl = press_at("ctrl_l", t_next);
        assert_eq!(dispatch_raw_event(&g, &mut tr, &next_ctrl), None);
        let next_shift = press_at("shift_l", t_next + Duration::from_millis(5));
        assert_eq!(
            dispatch_raw_event(&g, &mut tr, &next_shift),
            Some(TrackerOutput::ChordPress),
            "second PTT press after injection must fire — this is the #467 Windows regression"
        );
    }

    #[test]
    fn dispatch_verbose_distinguishes_guard_drop_from_tracker_decision() {
        // The diagnostic path needs to tell "guard swallowed" apart from
        // every tracker "no output" reason. Pin both sides.
        let g = InjectionGuard::new();
        let mut tr = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        let t0 = g.epoch;
        g.arm_at(t0, Duration::from_millis(200));

        // Armed → DroppedByGuard, and the tracker never sees the event
        // (verified indirectly: the follow-up ChordPress still fires
        // after the guard expires, meaning `pressed` was not polluted).
        let injected = press_at("__rdev_Unknown(231)", t0 + Duration::from_millis(10));
        assert_eq!(
            dispatch_verbose(&g, &mut tr, &injected),
            DispatchOutcome::DroppedByGuard
        );

        // Guard expired → Dispatched(TrackerDecision::ChordPress) for a
        // real user press.
        let t_after = t0 + Duration::from_millis(400);
        assert!(!g.is_active_at(t_after));
        let real = press_at("ctrl_l", t_after);
        assert_eq!(
            dispatch_verbose(&g, &mut tr, &real),
            DispatchOutcome::Dispatched(TrackerDecision::ChordPress)
        );
    }

    #[test]
    fn dispatch_forwards_events_when_guard_never_armed() {
        // Sanity: guard existence must not change tracker behaviour when
        // nothing has armed it. This protects the non-Windows platforms
        // (evdev / X11 without an active enigo path) where the guard is
        // constructed but never armed.
        let g = InjectionGuard::new();
        let mut tr = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        let t0 = g.epoch;
        assert_eq!(
            dispatch_raw_event(&g, &mut tr, &press_at("ctrl_l", t0)),
            Some(TrackerOutput::ChordPress)
        );
    }
}
