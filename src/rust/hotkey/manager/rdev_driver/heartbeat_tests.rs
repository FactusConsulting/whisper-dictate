//! heartbeat regression tests for the rdev driver.

use super::*;

// -----------------------------------------------------------------------
// #646 r3661145603 — bounded heartbeat log growth.
// -----------------------------------------------------------------------

#[test]
fn heartbeat_state_emits_and_never_retires_during_a_wedge() {
    // A pure wedge: every beat sees zero events. The first zero always
    // emits (transition into idle) and then every N-th zero emits — but
    // the thread NEVER retires, because retirement is only allowed after
    // a run of healthy beats.
    let mut state = HeartbeatState::default();
    let first = state.observe(0);
    assert!(first.emit, "first zero-event beat must emit (wedge signal)");
    assert!(!first.retire);
    // Feed enough zeros to well exceed HEARTBEAT_HEALTHY_QUOTA. None of
    // them may retire the thread.
    for _ in 0..(HEARTBEAT_HEALTHY_QUOTA + 100) {
        let a = state.observe(0);
        assert!(!a.retire, "zero-event beats must not retire the heartbeat");
    }
}

#[test]
fn heartbeat_state_retires_after_a_full_healthy_run() {
    // Simulate a genuinely healthy session: every beat carries events.
    // The thread must retire exactly at HEARTBEAT_HEALTHY_QUOTA.
    let mut state = HeartbeatState::default();
    for i in 1..HEARTBEAT_HEALTHY_QUOTA {
        let a = state.observe(1);
        assert!(a.emit, "healthy beats always emit");
        assert!(!a.retire, "must not retire before quota reached (i={i})");
    }
    let final_beat = state.observe(1);
    assert!(final_beat.emit);
    assert!(
        final_beat.retire,
        "must retire on the HEARTBEAT_HEALTHY_QUOTA-th healthy beat"
    );
}

#[test]
fn heartbeat_state_zero_beat_resets_the_healthy_run() {
    // A wedge that appears late (after some healthy beats) must reset the
    // healthy counter, so the thread does NOT retire during the possibly
    // wedged window. Otherwise the diagnostic signal we care most about
    // would disappear right when it matters.
    let mut state = HeartbeatState::default();
    for _ in 0..(HEARTBEAT_HEALTHY_QUOTA - 1) {
        let a = state.observe(1);
        assert!(!a.retire);
    }
    // One zero-event beat — the healthy counter resets. Even if the very
    // next beat is healthy, we should not retire (we need another full
    // healthy_quota run).
    let z = state.observe(0);
    assert!(z.emit, "the zero-event transition must emit");
    assert!(!z.retire);
    let post = state.observe(1);
    assert!(post.emit);
    assert!(
        !post.retire,
        "after a zero-event reset, retirement must wait for another full healthy run"
    );
}

#[test]
fn heartbeat_state_coalesces_idle_beats() {
    // On a genuinely idle session (nobody at the keyboard), the emit
    // cadence must fall to one line every HEARTBEAT_IDLE_EMIT_EVERY
    // beats — otherwise the tee file grows 12/min forever.
    //
    // Emit schedule with HEARTBEAT_IDLE_EMIT_EVERY = N and beats counted
    // by consecutive idle_run:
    //
    //   idle_run = 1               emit (active -> idle transition)
    //   idle_run = 2..N-1          coalesced (no emit)
    //   idle_run = N, 2N, 3N, ...  emit (every N-th)
    //
    // So for N = 10 the emitting beats are 1, 10, 20, ...
    let n = HEARTBEAT_IDLE_EMIT_EVERY;
    assert!(
        n >= 2,
        "coalesce test only meaningful for HEARTBEAT_IDLE_EMIT_EVERY >= 2"
    );
    let mut state = HeartbeatState::default();
    // idle_run = 1: emit (transition into idle).
    let first = state.observe(0);
    assert!(first.emit, "first zero-event beat must emit");
    // idle_run = 2..N-1: coalesced.
    for idle_run in 2..n {
        let a = state.observe(0);
        assert!(
            !a.emit,
            "idle beat {idle_run} within the coalesce window (2..{n}) must not emit"
        );
    }
    // idle_run = N: emit (every N-th idle beat).
    let nth = state.observe(0);
    assert!(nth.emit, "the N-th consecutive idle beat must emit");
    // idle_run = N+1..2N-1: coalesced again (N-1 beats).
    for idle_run in (n + 1)..(2 * n) {
        let a = state.observe(0);
        assert!(
            !a.emit,
            "idle beat {idle_run} within the second coalesce window must not emit"
        );
    }
    // idle_run = 2N: emit.
    let two_n = state.observe(0);
    assert!(two_n.emit, "the 2N-th consecutive idle beat must emit");
}

// -----------------------------------------------------------------------
// #657 r3663766095 — the heartbeat thread MUST actually exit
// when `stop` is set. The prior `spawn_startup_failure_stops_heartbeat_thread`
// test only asserted that `spawn` returned promptly, which was already
// true before the heartbeat_stop lifecycle fix — removing every stop
// store would leave the orphan heartbeat running while the test still
// returned by `spawn_heartbeat_thread`.
//
// Failure mode against the un-fixed code (the old fn ignored the
// spawn result with `let _ = ...`, so it had no way to expose a
// JoinHandle at all): the test would not compile — the import of
// `spawn_heartbeat_thread` (which used to be a private `fn`
// returning `()`) would surface it, and the `.join()` call below
// would type-check only against the new `Result<JoinHandle<()>>`
// return.
// -----------------------------------------------------------------------

#[test]
fn spawn_heartbeat_thread_exits_when_stop_is_signalled() {
    use std::time::{Duration, Instant};

    let total = Arc::new(AtomicU64::new(0));
    let since = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let handle = spawn_heartbeat_thread(Arc::clone(&total), Arc::clone(&since), Arc::clone(&stop))
        .expect("heartbeat thread must spawn on any test host");

    // Signal stop and give the sliced sleep in the heartbeat loop
    // (250 ms per slice) a bounded number of chances to observe it.
    // The heartbeat re-checks `stop` every 250 ms; a healthy exit
    // happens on the FIRST re-check after `store`, so 2 seconds is
    // ~8 slices of slack — comfortably above the sleep granularity
    // while still keeping the test suite fast.
    stop.store(true, Ordering::Relaxed);
    let join_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < join_deadline && !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        handle.is_finished(),
        "heartbeat thread must observe `stop` and exit within the deadline; \
         the pre-fix code never returned the JoinHandle so this invariant \
         could not be pinned — #657 r3663766095",
    );
    // Join to reap the thread + surface any panic that fired inside
    // the heartbeat body. `.join()` on a finished handle is
    // effectively free.
    handle.join().expect("heartbeat thread panicked");
}

#[test]
fn spawn_heartbeat_thread_reaches_in_loop_retirement_via_config_shim() {
    // #673 thread : the earlier
    // `spawn_heartbeat_thread_exits_on_retirement_even_without_external_stop`
    // test signalled `stop` from OUTSIDE the loop and therefore did not
    // exercise the retirement branch at all. Deleting the in-loop
    // `stop.store(true)` + `return` would have left it green while the
    // real self-retire path rotted.
    //
    // The parametrised `spawn_heartbeat_thread_with_config` shim lets
    // us drive the retirement branch on a millisecond timescale by
    // shrinking the interval and healthy-run quota. A background
    // "poker" thread primes `since` before each beat so `observe`
    // sees since>0, increments healthy_run, and eventually flips
    // `stop` from INSIDE the loop. The post-exit `stop.load()`
    // assertion is the key: only the in-loop `stop.store(true)` in
    // the retirement branch could have set it (we never set it from
    // this test).
    use std::time::{Duration, Instant};
    let total = Arc::new(AtomicU64::new(0));
    let since = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let handle = spawn_heartbeat_thread_with_config(
        Arc::clone(&total),
        Arc::clone(&since),
        Arc::clone(&stop),
        Duration::from_millis(5),
        3, // tiny quota
    )
    .expect("heartbeat thread must spawn");
    // Prime `since` before each beat so `observe` sees since>0 and
    // increments healthy_run. Poker thread pumps since=1 every 2ms.
    let since_poker = Arc::clone(&since);
    let poker = std::thread::spawn(move || {
        for _ in 0..500 {
            since_poker.store(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    // Wait for the in-loop self-retire to flip stop and exit.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        handle.is_finished(),
        "in-loop retirement branch must fire and exit within budget"
    );
    assert!(
        stop.load(Ordering::Relaxed),
        "the retirement branch's `stop.store(true)` MUST have been observed — \
         proves the internal path fired; a regression that removes that store \
         would leave `stop` false here even though the thread eventually exits \
         via the outer while-guard"
    );
    handle.join().expect("heartbeat panicked");
    poker.join().ok();
}
