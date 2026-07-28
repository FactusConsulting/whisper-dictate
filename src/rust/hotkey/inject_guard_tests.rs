//! Companion tests for [`crate::hotkey::inject_guard`].
//!
//! Extracted from an inline `#[cfg(test)] mod tests` in
//! `inject_guard.rs` so the regression-test discipline scanner (per
//! AGENTS.md `enforce-regression-test-discipline` — see
//! `src/tests/python/test_regression_test_discipline.py`) sees a
//! matching test file next to the production module. The inline layout
//! is not picked up by the scanner's "already-tested" exemption, which
//! resolves `foo.rs` → `foo_tests.rs` on the file system.
//!
//! The split was forced by the sonar gate on PR #668, which flagged
//! `clear_global_for_tests` as an untested new symbol when the
//! last-writer-wins `set_global` fix (Codex P2 #668 discussion
//! 3665741347) landed with its tests still inline. Same pattern as
//! `manager/rdev_driver_tests.rs` and `boot_self_test_tests.rs`.
//!
//! Because this file is wired via `#[cfg(test)] #[path = ...] mod ...`
//! from `hotkey/mod.rs`, it is a SIBLING of `inject_guard`, not a
//! child — so it reaches into the module by full path rather than
//! `use super::*`. Private-but-test-visible items (`arm_at`,
//! `epoch_for_tests`, `clear_global_for_tests`) are `pub(crate)` +
//! `#[cfg(test)]` on the production side for exactly this reason.

#![cfg(test)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::hotkey::inject_guard::{
    clear_global_for_tests, dispatch_raw_event, global, set_global, InjectionGuard,
};
use crate::hotkey::manager::tracker::{KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput};

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
    let t0 = g.epoch_for_tests();
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
    let t0 = g.epoch_for_tests();
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
    let t0 = g.epoch_for_tests();
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
    let t0 = g.epoch_for_tests();
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
    let t0 = g.epoch_for_tests();

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
    let t0 = g.epoch_for_tests();
    assert_eq!(
        dispatch_raw_event(&g, &mut tr, &press_at("ctrl_l", t0)),
        Some(TrackerOutput::ChordPress)
    );
}

// ------- Global guard slot -------

/// Take the CRATE-WIDE global-guard lock. Codex P2 #668 discussion
/// 3666165058: a lock private to this file would only serialise the
/// tests below — but `crate::hotkey::install_hotkey` also calls
/// `set_global` (before it even attempts listener startup), and
/// several tests in `hotkey/mod.rs` call it. One of those running in
/// parallel could replace the singleton between the
/// `set_global(g1)` / `set_global(g2)` pair and the `Arc::ptr_eq`
/// assertions, making `global_slot_last_writer_wins` flaky. The lock
/// lives in `crate::test_env_lock` so every caller across the crate
/// shares one instance.
fn global_slot_test_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::test_env_lock::GLOBAL_GUARD_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[test]
fn global_slot_last_writer_wins() {
    // Codex P2 #668 discussion 3665741347 changed `set_global` from
    // OnceLock (first-writer-wins) to a Mutex<Option<Arc<_>>>
    // (last-writer-wins). Rationale: on a supervisor
    // resume-failure fallback, the Python-worker install path
    // publishes a FRESH `InjectionGuard`, and the injector's
    // `global()` lookup MUST see that fresh guard — otherwise it
    // arms the stale guard while the new listener's callback
    // checks the fresh one, reproducing the exact self-injection
    // wedge the guard exists to prevent.
    //
    // Verifies via `Arc::ptr_eq` that the pointer returned by
    // `global()` matches the LAST call to `set_global`, not the
    // first. Pre-fix code would fail this test (it would return
    // the g1 pointer from the initial install).
    let _lock = global_slot_test_lock();
    clear_global_for_tests();
    let g1 = Arc::new(InjectionGuard::new());
    set_global(Arc::clone(&g1));
    let fetched_after_g1 = global().expect("g1 install must populate the slot");
    assert!(
        Arc::ptr_eq(&fetched_after_g1, &g1),
        "after the first install, global() must return g1"
    );

    let g2 = Arc::new(InjectionGuard::new());
    set_global(Arc::clone(&g2));
    let fetched_after_g2 = global().expect("g2 install must populate the slot");
    assert!(
        Arc::ptr_eq(&fetched_after_g2, &g2),
        "after the second install, global() MUST return g2 (the \
         fresh guard), not g1. Pre-fix first-writer-wins would \
         stubbornly return g1 and re-open the self-injection \
         wedge on supervisor resume-failure fallback. Codex P2 \
         #668 discussion 3665741347."
    );
    assert!(
        !Arc::ptr_eq(&fetched_after_g2, &g1),
        "the fresh guard must be a different Arc than the \
         original — sanity check that g1 and g2 aren't the same \
         pointer by test-construction accident"
    );

    clear_global_for_tests();
}

#[test]
fn global_slot_returns_none_when_never_set() {
    let _lock = global_slot_test_lock();
    clear_global_for_tests();
    assert!(global().is_none(), "an uninitialised slot must return None");
}

/// Codex P2 #668 discussion 3666165058 — the global-guard lock must be
/// CRATE-WIDE, not file-local, because `install_hotkey` publishes a
/// guard internally and lib tests call it.
///
/// Two invariants, both checked from source so a regression is caught
/// on any platform (the race itself is timing-dependent and would
/// otherwise only flake intermittently in CI):
///
/// 1. This file must take the shared `test_env_lock::GLOBAL_GUARD_LOCK`,
///    not define a private `static LOCK`.
/// 2. Every lib test that calls `install_hotkey(` must hold that lock,
///    because `install_hotkey` reaches `set_global` before its listener
///    startup can fail.
#[test]
fn global_guard_lock_is_crate_wide_and_held_by_install_hotkey_callers() {
    use std::fs;

    let tests_src = fs::read_to_string("src/rust/hotkey/inject_guard_tests.rs")
        .or_else(|_| fs::read_to_string("hotkey/inject_guard_tests.rs"))
        .expect("inject_guard_tests.rs must be readable from the crate root");
    assert!(
        tests_src.contains("test_env_lock::GLOBAL_GUARD_LOCK"),
        "the global-slot tests must serialise on the CRATE-WIDE \
         `test_env_lock::GLOBAL_GUARD_LOCK`, not a file-local static — \
         `install_hotkey` publishes a guard from other modules' tests \
         and would race these assertions. Codex P2 #668 3666165058."
    );

    // Any lib test that installs the hotkey subsystem publishes a
    // guard, so it must hold the same lock. `hotkey/mod.rs` is the
    // only lib-test module that calls `install_hotkey` on a path that
    // reaches `set_global` (the empty-config / unsupported-key tests
    // return Err during validation, before the publish).
    let mod_src = fs::read_to_string("src/rust/hotkey/mod.rs")
        .or_else(|_| fs::read_to_string("hotkey/mod.rs"))
        .expect("hotkey/mod.rs must be readable from the crate root");
    let installs = mod_src.matches("install_then_drive_coordinator_emits_actions_in_order");
    assert!(
        installs.count() > 0,
        "expected the install-and-drive test to still exist; if it was \
         renamed, update this scanner to match"
    );
    let test_start = mod_src
        .find("fn install_then_drive_coordinator_emits_actions_in_order()")
        .expect("install-and-drive test must exist");
    // Look at the first ~800 chars of the body — the lock must be
    // taken up-front, before any `install_hotkey` call.
    let window_end = (test_start + 900).min(mod_src.len());
    let body = &mod_src[test_start..window_end];
    let lock_idx = body.find("GLOBAL_GUARD_LOCK").unwrap_or(usize::MAX);
    let install_idx = body.find("install_hotkey(").unwrap_or(usize::MAX);
    assert!(
        lock_idx != usize::MAX,
        "`install_then_drive_coordinator_emits_actions_in_order` must \
         hold `GLOBAL_GUARD_LOCK` — `install_hotkey` publishes an \
         `InjectionGuard` into the process-global slot before listener \
         startup, so without the lock it races the global-slot \
         assertions in this file. Codex P2 #668 3666165058."
    );
    assert!(
        lock_idx < install_idx,
        "the lock must be acquired BEFORE `install_hotkey` is called, \
         otherwise the publish has already raced. Codex P2 #668 3666165058."
    );
}

#[test]
fn clear_global_for_tests_is_idempotent() {
    // `clear_global_for_tests` is the test-only unpublish half of the
    // replaceable-slot fix — it exists so a test isn't polluted by an
    // earlier test's `set_global` in the same binary. Calling it twice
    // in a row (or on an already-empty slot) must be a harmless no-op,
    // not a panic: the tests above bracket their work with it on both
    // ends, so a non-idempotent implementation would blow up the
    // second call in `global_slot_last_writer_wins`.
    let _lock = global_slot_test_lock();
    clear_global_for_tests();
    assert!(global().is_none());
    clear_global_for_tests();
    assert!(
        global().is_none(),
        "a second clear on an already-empty slot must stay empty \
         without panicking"
    );
    // And a clear AFTER a publish must actually drop the published
    // guard (not merely leave it in place) — otherwise cross-test
    // pollution would silently return.
    let g = Arc::new(InjectionGuard::new());
    set_global(Arc::clone(&g));
    assert!(global().is_some(), "publish must populate the slot");
    clear_global_for_tests();
    assert!(
        global().is_none(),
        "clear must actually drop the published guard so the next \
         test starts from a clean slot"
    );
}
