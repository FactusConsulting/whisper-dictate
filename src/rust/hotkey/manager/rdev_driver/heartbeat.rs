//! Heartbeat lifecycle and pure emit/retire policy.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{HEARTBEAT_HEALTHY_QUOTA, HEARTBEAT_IDLE_EMIT_EVERY, HEARTBEAT_INTERVAL};

/// Spawn the heartbeat thread. Runs until `stop` flips to `true`, which in
/// production never happens — the rdev listener itself cannot be joined so
/// there is no clean shutdown for the diagnostic layer above it either.
/// Test hosts flip the atomic to keep the tee file from growing across a
/// full unit-test run; production callers ignore the returned handle and
/// accept a process-lifetime thread.
///
/// Returns the [`std::thread::JoinHandle`] so a test can observe the thread
/// actually exits when `stop` is set — the pre-existing
/// `spawn_startup_failure_stops_heartbeat_thread` test could only assert
/// that `spawn` returned promptly, which was true even before the
/// heartbeat-stop lifecycle fix .
///
/// The thread name is set explicitly so a Windows dump / a `taskkill /f
/// /t` trace names it, and so `Thread::current().name()` in a future
/// panic hook can attribute stack traces to the right subsystem.
pub(crate) fn spawn_heartbeat_thread(
    events_total: Arc<AtomicU64>,
    events_since_heartbeat: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<thread::JoinHandle<()>> {
    spawn_heartbeat_thread_with_config(
        events_total,
        events_since_heartbeat,
        stop,
        HEARTBEAT_INTERVAL,
        HEARTBEAT_HEALTHY_QUOTA,
    )
}

/// Parametrised variant of [`spawn_heartbeat_thread`]. Production always
/// calls the constant-driven wrapper above; tests use this shim to reach
/// the in-loop `action.retire` branch on a millisecond timescale rather
/// than the production 60-minute one.
///
/// The earlier
/// `spawn_heartbeat_thread_exits_on_retirement_even_without_external_stop`
/// test signalled `stop` from OUTSIDE the loop and therefore did not
/// exercise the retirement branch at all. Deleting the in-loop
/// `stop.store(true)` + `return` would have left that test green while
/// the real self-retire path rotted.
///
/// The sleep-slice cap is `min(interval, 250 ms)` so millisecond-scale
/// test intervals don't over-sleep. Production callers with the 5-second
/// interval keep the pre-existing 250 ms slice cadence — no behaviour
/// change.
pub(crate) fn spawn_heartbeat_thread_with_config(
    events_total: Arc<AtomicU64>,
    events_since_heartbeat: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    interval: Duration,
    healthy_quota: u64,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("vp-hotkey-rdev-heartbeat".to_owned())
        .spawn(move || {
            // First heartbeat also emits an install-time marker so the
            // absolute t=<ms> value of the first `heartbeat` line pins
            // the listener-start moment even if the LL hook is silent.
            crate::diag::log!(
                "[hotkey/rdev] heartbeat thread started; interval={:?}",
                interval
            );
            let slice_cap = interval.min(Duration::from_millis(250));
            let mut state = HeartbeatState::with_healthy_quota(healthy_quota);
            while !stop.load(Ordering::Relaxed) {
                // Sleep in small slices so the stop signal is honoured
                // promptly in tests. A production process never toggles
                // stop, so the sliced sleep is a no-op there.
                let deadline = Instant::now() + interval;
                while Instant::now() < deadline {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let remaining = deadline
                        .saturating_duration_since(Instant::now())
                        .min(slice_cap);
                    thread::sleep(remaining);
                }
                let total = events_total.load(Ordering::Relaxed);
                let since = events_since_heartbeat.swap(0, Ordering::Relaxed);
                // Basic-level gate: users on `off` don't want a
                // heartbeat line every five seconds writing to their
                // gui-diagnostic.log — even one line per 5s is ~17 kB
                // per hour of accumulated log. The counter swap still
                // happens (so a later `basic` opt-in resumes with a
                // clean since=0 window) but no line lands.
                let action = state.observe(since);
                if crate::diag::info_enabled() && action.emit {
                    crate::diag::log!(
                        "[hotkey/rdev] listener heartbeat; events_since_last_heartbeat={since}; \
                         total_events={total}"
                    );
                }
                if action.retire {
                    // Retire even when info-gated: the emit-cap logic
                    // is independent of the log-level gate, and the
                    // retire message itself is a one-shot terminal
                    // marker that costs almost nothing.
                    if crate::diag::info_enabled() {
                        crate::diag::log!(
                            "[hotkey/rdev] heartbeat thread retiring after {} consecutive healthy \
                             beats - listener is confirmed alive and event-carrying. Stopping the \
                             beat bounds gui-diagnostic.log growth on long-running tray installs; \
                             a wedge after this point shows up in the tracker/coordinator \
                             diagnostics instead of the heartbeat.",
                            healthy_quota
                        );
                    }
                    stop.store(true, Ordering::Relaxed);
                    return;
                }
            }
        })
}

/// Pure decision state for the heartbeat thread — kept out of the sleep
/// loop so the emit / retire policy can be unit-tested without spawning
/// any threads or waiting real seconds. See [`HEARTBEAT_HEALTHY_QUOTA`]
/// and [`HEARTBEAT_IDLE_EMIT_EVERY`] for the policy documentation.
///
///
#[derive(Debug)]
pub(crate) struct HeartbeatState {
    /// Consecutive beats with `events_since > 0`. Once this hits
    /// [`Self::healthy_quota`] the thread retires. Any zero-event
    /// beat resets it to 0 — a wedge that appears late still gets full
    /// heartbeat coverage.
    healthy_run: u64,
    /// Consecutive beats with `events_since == 0` since the last write.
    /// The wedge-detection signal remains: the first zero after activity
    /// always writes, subsequent quiet beats coalesce until
    /// [`HEARTBEAT_IDLE_EMIT_EVERY`], keeping the tee file bounded on
    /// truly idle sessions.
    idle_run: u64,
    /// Number of consecutive healthy beats required before the thread
    /// retires. Production uses [`HEARTBEAT_HEALTHY_QUOTA`]; tests use
    /// a tiny value via [`Self::with_healthy_quota`] so the retirement
    /// path can be exercised in milliseconds instead of the ~60-minute
    /// production window.
    healthy_quota: u64,
}

impl Default for HeartbeatState {
    fn default() -> Self {
        Self::with_healthy_quota(HEARTBEAT_HEALTHY_QUOTA)
    }
}

/// What the heartbeat thread should do after one beat's counter read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct HeartbeatAction {
    pub emit: bool,
    pub retire: bool,
}

impl HeartbeatState {
    /// Build a state that retires after `healthy_quota` consecutive
    /// healthy beats. Used both by the [`Default`] impl (which passes
    /// [`HEARTBEAT_HEALTHY_QUOTA`]) and by
    /// [`spawn_heartbeat_thread_with_config`] so tests can trip the
    /// retirement branch on a millisecond timescale

    pub(crate) fn with_healthy_quota(healthy_quota: u64) -> Self {
        Self {
            healthy_run: 0,
            idle_run: 0,
            healthy_quota,
        }
    }

    /// Update the healthy/idle counters and return whether this beat
    /// should emit and/or retire the thread.
    ///
    /// Emit rule: a beat writes when `since > 0` (activity moved — the
    /// operator cares about the number) OR every
    /// [`HEARTBEAT_IDLE_EMIT_EVERY`]th consecutive zero beat (the "hook
    /// still up on an idle session" signal). The first zero after
    /// activity is not a beat-# multiple of the emit-every count, so
    /// we force-emit on the transition from active → idle to keep the
    /// wedge signal responsive.
    ///
    /// Retire rule: [`HEARTBEAT_HEALTHY_QUOTA`] consecutive `since > 0`
    /// beats means the listener has demonstrably been carrying events
    /// for the whole observation window — the investigation is over,
    /// and the healthy-case log growth cap kicks in.
    pub(crate) fn observe(&mut self, since: u64) -> HeartbeatAction {
        if since > 0 {
            let transitioned_from_idle = self.idle_run > 0;
            self.idle_run = 0;
            self.healthy_run = self.healthy_run.saturating_add(1);
            let retire = self.healthy_run >= self.healthy_quota;
            // Always emit on an active beat AND on the first active
            // beat after an idle run (the log reader wants to see the
            // resumption timestamp). The unconditional emit here is a
            // superset of the transition case; kept as one branch for
            // clarity.
            let _ = transitioned_from_idle;
            HeartbeatAction { emit: true, retire }
        } else {
            // Zero-event beat: potential wedge signal. Reset the
            // healthy-run counter so we do NOT retire during a possibly
            // wedged window.
            self.healthy_run = 0;
            self.idle_run = self.idle_run.saturating_add(1);
            // Emit on the first zero (the active -> idle transition, so
            // the operator sees the timestamp) and then once every
            // HEARTBEAT_IDLE_EMIT_EVERY beats — a genuinely idle session
            // still writes at ~1/(N * HEARTBEAT_INTERVAL) instead of the
            // unbounded 12/min pre-cap cadence.
            let emit =
                self.idle_run == 1 || self.idle_run.is_multiple_of(HEARTBEAT_IDLE_EMIT_EVERY);
            HeartbeatAction {
                emit,
                retire: false,
            }
        }
    }
}
