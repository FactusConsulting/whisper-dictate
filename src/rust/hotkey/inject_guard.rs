//! Self-injection guard — filters the OS key events our own text injector
//! synthesises out of the PTT tracker's input stream.
//!
//! ## Why this exists (Windows PTT wedge)
//!
//! On Windows the [`crate::dictate::backends::EnigoInjectBackend`] injector
//! reaches the OS via `SendInput`. Those synthetic events flow through
//! **every** `WH_KEYBOARD_LL` hook — including the one `rdev` installs for
//! the PTT listener — because rdev 0.5's callback does not inspect
//! `KBDLLHOOKSTRUCT.flags & LLKHF_INJECTED`. The consequence: every
//! character the app types after a transcription feeds back into the PTT
//! tracker, along with the
//! [`crate::dictate::backends::inject::STALE_MODIFIER_VKS`] release sweep
//! (`VK_SHIFT`, `VK_CONTROL`, `VK_LWIN`, …) — some of which rdev DOES
//! resolve to real names (`shift_r`, `ctrl_r`, `alt_gr`, `cmd_l`, …). That
//! stream can leave the tracker's `pressed` map populated with stray
//! foreign keys, tripping bare-modifier rule 1 for the *next* PTT press —
//! which then never fires until the 10 s foreign-key self-heal expires.
//! Symptom the user reports: **"PTT works once, then can't be activated
//! again"**.
//!
//! Same class of bug as #467 on Linux/Wayland, where the fix was to
//! exclude the `ydotoold` virtual `/dev/input` node from the evdev
//! listener's device enumeration (that channel is device-level; Windows
//! has no equivalent). Here we filter at the event-stream layer: the
//! injector *brackets* the guard around every `SendInput` burst, and the
//! rdev driver's callback drops every event that arrives while the guard
//! is active.
//!
//! ## Timing model — bracket + monotonic-forward grace horizon
//!
//! Two complementary mechanisms so both the burst itself AND the LL-hook
//! drain tail are covered:
//!
//! * A **bracket counter** ([`InjectionGuard::arm_start`] +
//!   [`InjectionGuard::arm_end`]) that is `> 0` for the exact duration of
//!   the `SendInput` sequence. This is what makes multi-second bursts
//!   safe — a long enigo typing loop keeps the counter positive
//!   throughout, so `is_active` stays true no matter how long the burst
//!   takes. The original PR #476 used only a fixed pre-arm window (50 ms)
//!   which leaked when the burst outran the grace, per Codex review.
//!
//! * A **monotonic-forward horizon** ([`InjectionGuard::active_until`]
//!   tick) covering the pre-arm buffer (before the counter goes up so
//!   the very first LL-hook event catches a raised guard) AND the
//!   post-arm grace after the counter drops (WH_KEYBOARD_LL events
//!   reach rdev's callback via the installing thread's message pump,
//!   which runs on a different thread than the injector and can trail
//!   `SendInput`'s return by tens to a couple-hundred milliseconds under
//!   load). The horizon only ever moves forward — a late short arm
//!   cannot pull it backwards past an earlier long arm still in flight.
//!
//! `is_active()` returns true iff either (counter > 0) OR (horizon > now).
//! Production grace values are 50 ms pre-arm + 200 ms post-arm — see
//! [`crate::dictate::backends::inject`].
//!
//! ## Hot-path budget — zero allocation when inactive
//!
//! The rdev listener callback runs on the OS's LL-hook thread and gets
//! called for **every** keydown/keyup on the entire desktop. It MUST NOT
//! allocate on that path when the guard is inactive (which is ≈99.9 % of
//! the time). PR #478 (diagnostic instrumentation) shipped per-event
//! allocation and produced a mouse-freeze regression on Windows; that
//! must not recur.
//!
//! The check on the hot path is exactly two atomic loads
//! (`active_brackets`, then `active_until_millis`) and one saturating
//! `Duration` arithmetic op on the caller-supplied [`Instant`]. No heap,
//! no lock, no string formatting. See [`InjectionGuard::is_active_at`].
//!
//! ## Testability
//!
//! The guard is a pure `AtomicUsize` + `AtomicU64` + `Instant` epoch —
//! no globals, no I/O, no threads — so its bracket / horizon semantics
//! are unit-tested directly here. Production wiring plumbs an
//! `Arc<InjectionGuard>` from [`super::install_hotkey`] into both the
//! rdev driver's callback and the injector wrapper; tests can construct
//! their own guard and drive the driver's [`dispatch_raw_event`] helper
//! without spawning any OS listener.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::manager::tracker::{KeyTracker, RawKeyEvent, TrackerOutput};

/// Process-wide self-injection guard. Cloneable through `Arc` — one
/// instance is created per `install_hotkey` call and shared with both the
/// hotkey driver's callback and the injector wrapper. See the module doc
/// for the timing rationale.
///
/// State is two disjoint pieces:
///
/// * `active_brackets` — count of `arm_start` calls that have not yet
///   been matched by `arm_end`. `> 0` means "we are inside a SendInput
///   burst right now, no matter how long it takes".
/// * `active_until_millis` — monotonic-non-decreasing "no events before"
///   tick count relative to a fixed epoch captured at construction.
///   Covers the pre-arm buffer (so the very first LL-hook event finds
///   the guard raised) and the post-arm grace (so the LL-hook drain
///   tail after `arm_end` is still dropped).
///
/// No explicit disarm on the horizon — it decays on its own so a
/// forgotten `arm_end` (a panic mid-burst) cannot wedge the listener
/// forever. The counter would leak in that case, but the burst path
/// uses `arm_end` in the same function as `arm_start` with no `?`
/// early-return between them, so a leak is only reachable via a real
/// panic — at which point the whole process is unhealthy anyway.
#[derive(Debug)]
pub struct InjectionGuard {
    /// Count of currently-open `arm_start` brackets. Guard is active
    /// while this is `> 0` regardless of the horizon — this is what
    /// makes multi-second injection bursts safe (Codex feedback on the
    /// original PR #476: a fixed pre-arm window leaks when the burst
    /// outruns the grace).
    active_brackets: AtomicUsize,
    /// Milliseconds since [`Self::epoch`] before which any observed OS
    /// key event is treated as self-injected. `0` means "never armed".
    /// Only ever moves forward.
    active_until_millis: AtomicU64,
    /// Monotonic reference point for [`Self::active_until_millis`].
    /// Captured once at construction so `arm` / `is_active` do not
    /// depend on wall-clock time (which can jump backwards).
    epoch: Instant,
}

impl InjectionGuard {
    /// Build a fresh (inactive) guard. Cheap — no I/O, no allocations
    /// besides the containing `Arc` at the call site.
    pub fn new() -> Self {
        Self {
            active_brackets: AtomicUsize::new(0),
            active_until_millis: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    /// True iff a self-injection burst is currently in progress OR the
    /// post-burst grace window has not yet elapsed. Called from the
    /// hotkey driver's callback on every OS event to decide whether to
    /// forward the event to the tracker.
    ///
    /// **Hot path.** MUST NOT allocate — see the module doc's
    /// "Hot-path budget" section. Two `Ordering::Relaxed` atomic loads
    /// and a saturating `Duration` op. Ordering::Relaxed is sufficient
    /// because the guard is intentionally best-effort: a race where the
    /// callback reads the counter/horizon just before the injector
    /// arms it lets at most one event slip through, and the injector
    /// then arms with a 50 ms pre-grace anyway.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.is_active_at(Instant::now())
    }

    /// [`Self::is_active`] with an injected `now` — used by the rdev
    /// callback so the check happens against the event's own timestamp
    /// (avoids a redundant `Instant::now()` per event), and by unit
    /// tests to probe the horizon without waiting real wall-clock time.
    #[inline]
    pub fn is_active_at(&self, now: Instant) -> bool {
        // Fast path: any open bracket means we're inside a burst.
        // Checked FIRST because it's the definitively-true case for the
        // whole burst duration — no arithmetic needed.
        if self.active_brackets.load(Ordering::Relaxed) > 0 {
            return true;
        }
        let now_ms = now.saturating_duration_since(self.epoch).as_millis() as u64;
        self.active_until_millis.load(Ordering::Relaxed) > now_ms
    }

    /// Open a bracket around an injection burst and extend the horizon
    /// by `pre_grace`. Called by the injector wrapper **immediately
    /// before** it starts issuing `SendInput` calls, so:
    ///
    /// * the counter goes up, keeping [`Self::is_active`] true for the
    ///   whole burst regardless of how long it takes, and
    /// * the horizon covers the microseconds between this call and the
    ///   very first `SendInput` (a fast machine can dispatch the first
    ///   LL-hook event in single-digit microseconds so we don't want a
    ///   race between "arm the counter" and "issue SendInput").
    ///
    /// Every `arm_start` MUST be matched by exactly one `arm_end` —
    /// see the type doc for the panic caveat.
    pub fn arm_start(&self, pre_grace: Duration) {
        // Increment BEFORE extending the horizon so a concurrent
        // `is_active` on another thread sees the counter positive as
        // soon as any effect of `arm_start` is visible.
        self.active_brackets.fetch_add(1, Ordering::SeqCst);
        self.extend_horizon(Instant::now(), pre_grace);
    }

    /// Close a bracket opened by [`Self::arm_start`] and extend the
    /// horizon by `post_grace`. Called by the injector wrapper
    /// **immediately after** the last `SendInput` returns, so the
    /// LL-hook drain tail (WH_KEYBOARD_LL events can trail `SendInput`
    /// by tens to a couple-hundred milliseconds on the callback thread)
    /// is still dropped even after the counter drops to zero.
    ///
    /// Order of operations is: extend horizon FIRST, then decrement the
    /// counter. That way any brief window where the counter goes to
    /// zero but the LL-hook has not yet drained is covered by the
    /// horizon we just wrote.
    pub fn arm_end(&self, post_grace: Duration) {
        self.extend_horizon(Instant::now(), post_grace);
        // Saturating in case a caller managed to over-close (which
        // would only happen through a bug — but we don't want to
        // panic the hotkey subsystem for that).
        let prev = self.active_brackets.load(Ordering::SeqCst);
        if prev == 0 {
            debug_assert!(false, "arm_end called without matching arm_start");
            return;
        }
        self.active_brackets.fetch_sub(1, Ordering::SeqCst);
    }

    /// Extend the "no events before" horizon to at least `now + grace`.
    /// Never shortens — a late arm with a small grace cannot pull the
    /// horizon backwards past an earlier long-grace arm still in flight
    /// (that would let injected tail events leak through).
    ///
    /// This is the primitive both [`Self::arm_start`] /
    /// [`Self::arm_end`] and the compatibility [`Self::arm`] use.
    fn extend_horizon(&self, now: Instant, grace: Duration) {
        let now_ms = now.saturating_duration_since(self.epoch).as_millis() as u64;
        let new_until = now_ms.saturating_add(grace.as_millis() as u64);
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

    /// Legacy horizon-only arm — extends the horizon without touching
    /// the bracket counter. Retained for the direct-arm unit tests that
    /// pin the monotonic-forward semantics without going through the
    /// bracket path. Production code should use
    /// [`Self::arm_start`] / [`Self::arm_end`].
    #[cfg(test)]
    fn arm_at(&self, now: Instant, grace: Duration) {
        self.extend_horizon(now, grace);
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
/// **Hot path** — no allocations when `guard` is inactive (see
/// `InjectionGuard::is_active_at`).
#[inline]
pub fn dispatch_raw_event(
    guard: &InjectionGuard,
    tracker: &mut KeyTracker,
    event: &RawKeyEvent,
) -> Option<TrackerOutput> {
    if guard.is_active_at(event.at) {
        return None;
    }
    tracker.handle(event)
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

    // ------- InjectionGuard: horizon arm / decay semantics -------

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
        // Simulates "long pre-arm + short post-arm": the short arm must
        // not pull the horizon backwards or the LL-hook tail would leak.
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

    // ------- Bracket counter semantics (arm_start / arm_end) --------

    #[test]
    fn bracket_open_keeps_guard_active_indefinitely() {
        // The core reason the bracket exists: a multi-second injection
        // burst outruns any fixed pre-arm grace. As long as the bracket
        // is open, `is_active` returns true no matter how far the
        // clock has advanced.
        let g = InjectionGuard::new();
        g.arm_start(Duration::from_millis(50));
        // Even if the "current time" is way past the pre-grace horizon,
        // the open bracket keeps the guard raised. We rely on the
        // real-clock version here because the bracket check is
        // clock-independent.
        assert!(
            g.is_active(),
            "open bracket must keep guard active regardless of horizon"
        );
        g.arm_end(Duration::from_millis(200));
    }

    #[test]
    fn arm_end_leaves_horizon_covering_post_grace() {
        // After the bracket closes, `is_active` remains true for the
        // post-arm grace window so the LL-hook drain tail is still
        // dropped. The horizon covers this because `arm_end` extended
        // it before decrementing the counter.
        let g = InjectionGuard::new();
        g.arm_start(Duration::from_millis(50));
        // Instant briefly captured — after arm_end returns, the
        // horizon covers `arm_end_time + post_grace`.
        g.arm_end(Duration::from_millis(200));
        // Counter is now 0, so is_active depends purely on the horizon.
        // Should still be active immediately after arm_end.
        assert!(
            g.is_active(),
            "guard must stay active during post-arm grace"
        );
        // After the grace elapses, the guard decays.
        std::thread::sleep(Duration::from_millis(250));
        assert!(!g.is_active(), "guard must decay after post-arm grace");
    }

    #[test]
    fn nested_brackets_stay_active_until_all_close() {
        // Two overlapping bursts (unusual but possible if the injector
        // is ever called re-entrantly). The counter approach means both
        // must close before the guard drops.
        let g = InjectionGuard::new();
        g.arm_start(Duration::from_millis(50));
        g.arm_start(Duration::from_millis(50));
        assert!(g.is_active());
        g.arm_end(Duration::from_millis(200));
        // Still one bracket open — guard remains raised on the counter.
        assert!(
            g.is_active(),
            "guard must stay active while outer bracket still open"
        );
        g.arm_end(Duration::from_millis(200));
        // Both closed — the horizon takes over (post-grace still covers
        // this instant).
        assert!(g.is_active(), "post-grace horizon still covers us");
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
    fn dispatch_drops_events_inside_open_bracket_regardless_of_horizon() {
        // The bracket-specific regression: even for a burst that has
        // outrun the pre-grace horizon (a long enigo typing loop), the
        // open bracket must still cause dispatch to drop events.
        let g = InjectionGuard::new();
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned(), "shift_l".to_owned()]);
        g.arm_start(Duration::from_millis(1)); // deliberately tiny grace
                                               // Sleep past the horizon so only the bracket keeps us active.
        std::thread::sleep(Duration::from_millis(20));
        // `at` is real-clock-now (which is now past the pre-grace
        // horizon) — the bracket alone must filter this event out.
        let injected = press_at("__rdev_Unknown(231)", Instant::now());
        assert_eq!(
            dispatch_raw_event(&g, &mut t, &injected),
            None,
            "open bracket must drop events even after pre-grace horizon expires"
        );
        g.arm_end(Duration::from_millis(200));
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

    // ------- Global guard slot -------

    #[test]
    fn global_slot_first_writer_wins() {
        // `set_global` is idempotent — subsequent writers silently lose
        // (a test host that installs the hotkey subsystem twice must not
        // panic).
        let g1 = Arc::new(InjectionGuard::new());
        set_global(Arc::clone(&g1));
        let g2 = Arc::new(InjectionGuard::new());
        set_global(Arc::clone(&g2));
        // The exact pointer depends on test ordering (other tests in the
        // same binary may have populated the slot first) — we can only
        // assert `global()` returns *some* guard now.
        assert!(global().is_some(), "global slot must be populated");
    }
}
