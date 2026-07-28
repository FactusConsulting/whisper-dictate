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
use std::sync::{Arc, Mutex, OnceLock};
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
    ///
    /// `pub(crate)` so the companion `inject_guard_tests.rs` (a sibling
    /// module, not a child, because it is wired via `#[path]` from
    /// `hotkey/mod.rs`) can reach it. Still `#[cfg(test)]`, so the
    /// shipping surface is unchanged.
    #[cfg(test)]
    pub(crate) fn arm_at(&self, now: Instant, grace: Duration) {
        self.extend_horizon(now, grace);
    }

    /// Test-only accessor for the guard's monotonic reference point.
    /// The field itself stays private — the companion test file needs
    /// a stable `t0` to build deterministic `Instant` offsets from, and
    /// exposing a reader keeps that seam narrower than making the field
    /// `pub(crate)`.
    #[cfg(test)]
    pub(crate) fn epoch_for_tests(&self) -> Instant {
        self.epoch
    }

    /// Snapshot of the open-bracket count. Used by the
    /// injection-idempotency self-test to prove the counter returns to
    /// zero after every simulated burst — a leak here is the "unbalanced
    /// arm_end" bug class the harness is designed to catch (see
    /// [`crate::injection::self_test`]).
    ///
    /// Not on the hot path — a `Relaxed` load is fine here because the
    /// caller pairs this with a bracket drop that already ordered its
    /// own decrement with `SeqCst`.
    #[inline]
    pub fn active_brackets(&self) -> usize {
        self.active_brackets.load(Ordering::Relaxed)
    }
}

impl Default for InjectionGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII wrapper around [`InjectionGuard::arm_start`] /
/// [`InjectionGuard::arm_end`]. Opening the bracket immediately arms
/// the guard's counter (keeping it active for the whole burst, no
/// matter how many seconds a long typing loop takes); dropping the
/// bracket extends the horizon by `post_grace` and decrements the
/// counter.
///
/// Using RAII rather than a manual `arm_end` call means an early
/// return (`?` on the inject result), or a panic inside the burst,
/// still closes the bracket cleanly — otherwise a leaked counter
/// would keep PTT deaf for the rest of the process's lifetime.
///
/// The type holds a bare `&InjectionGuard` reference rather than an
/// `Arc` clone so callers can keep their `Option<Arc<_>>` alive for
/// the full lifetime of the borrow without an extra atomic-refcount
/// bump per inject.
///
/// This is the *single* bracket primitive used by both the shipping
/// `EnigoInjectBackend::inject` path (via the [`crate::dictate`] wrapper
/// with production `INJECT_PRE_GRACE` / `INJECT_POST_GRACE` constants)
/// AND the `self-test injection-idempotency` regression harness (see
/// [`crate::injection::self_test`]) — the whole point of exposing it
/// here is that the self-test exercises the *same* RAII bracket the
/// real inject path uses, not a hand-rolled arm_start/arm_end pair
/// that could drift.
pub struct InjectionBracket<'a> {
    guard: &'a InjectionGuard,
    post_grace: Duration,
}

impl<'a> InjectionBracket<'a> {
    /// Open a bracket around a burst. `pre_grace` is applied
    /// immediately (via [`InjectionGuard::arm_start`]); `post_grace`
    /// is stashed and applied on drop.
    pub fn open(guard: &'a InjectionGuard, pre_grace: Duration, post_grace: Duration) -> Self {
        guard.arm_start(pre_grace);
        Self { guard, post_grace }
    }
}

impl Drop for InjectionBracket<'_> {
    fn drop(&mut self) {
        self.guard.arm_end(self.post_grace);
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

/// Process-wide slot for the currently-installed injection guard.
///
/// Was `OnceLock<Arc<InjectionGuard>>` (first-writer-wins) until
/// Codex P2 #668 discussion 3665741347 pointed out that the
/// supervisor's failed-resume fallback path (which clears the dead
/// `hotkey_handle` and lets the Python-worker path install a fresh
/// listener with a NEW `InjectionGuard`) would leave the injector
/// arming the STALE guard from the first install while the new
/// listener's callback checked the fresh guard. Injected transcript
/// keystrokes then bypassed the tracker's self-injection filter and
/// reproduced the exact "PTT works once, then wedges" failure the
/// guard is meant to prevent.
///
/// A `Mutex<Option<Arc<...>>>` slot lets `set_global` REPLACE the
/// current guard on every install, so the injector's `global()`
/// lookup always returns the same guard the current listener callback
/// is checking. Cheap: contention is startup-only (install path) and
/// per-inject (`global()` clone under a short lock), and the injector
/// already pays a call-per-inject overhead for its normal path.
static GLOBAL_INJECTION_GUARD: OnceLock<Mutex<Option<Arc<InjectionGuard>>>> = OnceLock::new();

fn global_slot() -> &'static Mutex<Option<Arc<InjectionGuard>>> {
    GLOBAL_INJECTION_GUARD.get_or_init(|| Mutex::new(None))
}

/// Publish `guard` as the process-wide injection guard. REPLACES any
/// previously-published guard so a supervisor reinstall (e.g. Phase-B
/// resume-fallback landing on the Python-worker install path) sees a
/// consistent guard between the listener callback and the injector's
/// `arm()` call. Codex P2 #668 discussion 3665741347.
///
/// Production `install_hotkey` runs at most once per install pass —
/// the `Mutex` is contended only for that startup moment. Tests that
/// install multiple times in a single process (integration harness,
/// this module's own test suite) now observe the LATEST install,
/// which is closer to the real behaviour the fix is preserving.
pub fn set_global(guard: Arc<InjectionGuard>) {
    if let Ok(mut slot) = global_slot().lock() {
        *slot = Some(guard);
    }
}

/// Fetch the process-wide injection guard, if any [`set_global`]
/// caller has populated it. Returns a cheap `Arc` clone. Used by the
/// injector's arm-around-SendInput fallback path (see
/// `crate::dictate::backends::EnigoInjectBackend::inject`).
pub fn global() -> Option<Arc<InjectionGuard>> {
    global_slot().lock().ok().and_then(|s| s.clone())
}

/// Test-only helper: drop any previously-published global guard so a
/// fresh test isn't polluted by an earlier test's install. Not
/// exposed on the shipping API — production callers only ever
/// publish, never unpublish.
#[cfg(test)]
pub(crate) fn clear_global_for_tests() {
    if let Ok(mut slot) = global_slot().lock() {
        *slot = None;
    }
}

// Unit tests moved to the companion `inject_guard_tests.rs` file so the
// regression-test discipline scanner (per AGENTS.md, see
// `src/tests/python/test_regression_test_discipline.py`) sees a matching
// test file next to the production module. Sonar quality-gate feedback
// on PR #668 required the split: an inline `#[cfg(test)] mod tests` in
// this file did not satisfy the scanner's `foo.rs` -> `foo_tests.rs`
// lookup when `clear_global_for_tests` was introduced by the
// last-writer-wins `set_global` fix (Codex P2 #668 discussion 3665741347).
