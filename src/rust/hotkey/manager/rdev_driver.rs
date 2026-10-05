//! `rdev` driver layer — only compiled when the `rust-hotkeys` feature is on.
//!
//! Two threads per subsystem:
//!
//! * the *listener* thread, which calls `rdev::listen` and blocks forever
//!   (rdev has no clean stop API), translating each `rdev::Event` into a
//!   [`super::tracker::RawKeyEvent`] and feeding the shared [`super::tracker::KeyTracker`];
//! * the *manager* thread, which owns the `Mutex<KeyTracker>` and processes
//!   register/unregister commands sent over an mpsc.
//!
//! Since PR #644, a third thread (the *heartbeat* thread) also runs and
//! logs periodic diagnostics — see [`spawn_heartbeat_thread`] and the
//! Windows PTT wedge story in the module-level `HEARTBEAT` docs below.
//!
//! The two production threads are split because `rdev::listen` is not
//! `Send`, blocks the thread it runs on for the process lifetime, and
//! offers no register/unregister API of its own — so the manager thread
//! is the only place from which the rest of the runtime can safely talk
//! to the binding.
//!
//! ## Listener readiness
//!
//! `rdev::listen` returns `Result<(), ListenError>`. On platforms where the
//! global hook can be installed (Windows / macOS with accessibility / X11
//! with a display), it blocks forever on success; on Linux without an X
//! display, or macOS without accessibility, it returns `Err` quickly. The
//! driver therefore signals the spawning thread (a) immediately, that the
//! thread is up, and (b) again if `listen` returns Err. [`spawn`] waits up
//! to [`READY_PROBE_WINDOW`] for an error after seeing the "started" signal
//! — if no error arrives the listener is treated as healthy and `spawn`
//! returns. This is what surfaces "rdev never made it past listen()" to the
//! caller of `install_hotkey()` so the supervisor can keep the alternate
//! listener wired instead of parking it.
//!
//! ## Heartbeat instrumentation (Windows PTT wedge diagnostic)
//!
//! The Windows GUI (`whisper-dictate-gui.exe`) has `windows_subsystem =
//! "windows"` and no attached console. The `%LOCALAPPDATA%\WhisperDictate\
//! gui-diagnostic.log` tee added in PR #644 shows that Phase-B install
//! runs, but users report that pressing the configured chord never fires
//! a session. To distinguish the three possible root causes without a
//! second bug-report round-trip, this driver ships two complementary
//! diagnostics:
//!
//! 1. A **heartbeat thread** logs `[hotkey/rdev] listener heartbeat;
//!    events_since_last_heartbeat=N; total_events=T` every
//!    [`HEARTBEAT_INTERVAL`]. The counters are updated by the LL-hook
//!    callback, so a heartbeat of `events_since_last_heartbeat=0` in the
//!    same window that the user was pressing keys narrows the fault to
//!    the OS listener path (message pump not delivering to the callback,
//!    or hook never installed by rdev), not the tracker or coordinator.
//!    On healthy sessions the thread retires after
//!    [`HEARTBEAT_HEALTHY_QUOTA`] consecutive event-carrying beats so
//!    an always-on tray install does not accumulate log noise
//! indefinitely . Zero-event beats keep
//!    the wedge signal alive and are coalesced to one line every
//!    [`HEARTBEAT_IDLE_EMIT_EVERY`] beats to bound growth on idle
//!    sessions too.
//! 2. A **rate-limited per-event trace**: the first
//!    [`RAW_EVENT_INITIAL_TRACE`] events always log, then every
//!    [`RAW_EVENT_TRACE_EVERY`]th event. This surfaces the actual key
//!    names rdev delivers so a chord-matcher rejection ("hook is alive
//!    but the event is called `ctrl` not `ctrl_l`", or "AltGr shows up
//!    as `alt_gr` but the user configured `alt_r`") is visible without
//!    a rebuild.

use std::time::Duration;

pub use super::driver_common::{
    ManagerCommand, ManagerHandle, ManagerThread, NoopRawTap, RawTap, SpawnError,
};

mod callback;
mod diagnostics;
mod heartbeat;
mod keys;
mod listener;
mod listener_thread;
mod readiness;

pub use keys::is_rdev_supported_name;
#[cfg(test)]
pub use listener::spawn;
pub use listener::spawn_with_raw_tap;

#[cfg(test)]
pub(crate) use diagnostics::ensure_callback_trace_writer_for_tests;
#[cfg(test)]
pub(crate) use listener::spawn_with_raw_tap_capturing_heartbeat_for_tests;
#[allow(unused_imports)]
pub(crate) use {
    diagnostics::{
        enqueue_callback_trace, redact_event_type_for_debug, redact_raw_event_name,
        should_log_raw_event, CALLBACK_TRACE_QUEUE_CAPACITY,
    },
    heartbeat::{spawn_heartbeat_thread, spawn_heartbeat_thread_with_config, HeartbeatState},
    keys::raw_from_rdev,
    readiness::{
        await_listener_ready, listener_readiness_handshake, ListenerSignal, ListenerStart,
    },
};

/// Maximum time [`spawn`] waits after the listener thread reports "started"
/// for `rdev::listen` to either return Err (and thus be a startup failure)
/// or stay blocked (and thus be healthy). Tuned for fast-failure platforms
/// like headless Linux without making CI slow.
const READY_PROBE_WINDOW: Duration = Duration::from_millis(250);

/// How often the heartbeat thread emits its `[hotkey/rdev] listener
/// heartbeat; ...` line. Five seconds is short enough that a user testing
/// PTT interactively will see fresh output within one press cycle, and long
/// enough that the tee file does not grow noticeably during quiet periods
/// (12 lines per minute; ~17 kB per hour of steady-state runtime).
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// Number of leading raw events that always emit a `[hotkey/rdev] raw
/// event #N: ...` trace line. Ensures the very first keystroke after a
/// suspected-wedged install produces a log entry — without this a user
/// who presses ctrl_l exactly once at startup would see the heartbeat's
/// non-zero counter but no name/kind of the event.
const RAW_EVENT_INITIAL_TRACE: u64 = 10;

/// After the leading window, only every N-th raw event logs. Balances
/// "we can see forward progress in a long session" against "typing
/// bursts don't flood the tee". 100 → one trace line per typical
/// dictation utterance in steady-state use.
const RAW_EVENT_TRACE_EVERY: u64 = 100;

/// How many consecutive heartbeats with `events_since_last_heartbeat > 0`
/// mark the listener as healthy and let the heartbeat thread retire. One
/// hour at [`HEARTBEAT_INTERVAL`] = 5 s cadence (12 × 60 = 720 healthy
/// beats) — long enough that the diagnostic value has clearly landed
/// (every user-visible startup, plus the "hook still alive after an hour
/// of steady use" signal), short enough that an always-on tray install
/// on a low-typing day does not accumulate hundreds of MB of dumb-log
/// noise. A single zero-event beat during the window resets the counter
/// so a wedge that appears late still gets full heartbeat coverage.
///
/// discussion r3661145603.
pub(crate) const HEARTBEAT_HEALTHY_QUOTA: u64 = 720;

/// Fallback emit cadence for healthy heartbeats — during the observation
/// window a beat only writes when `events_since > 0` OR when this many
/// consecutive zero-event beats have elapsed (whichever comes first).
/// Keeps the "hook still up" signal alive on genuinely idle sessions
/// (nobody at the keyboard) while suppressing minute-by-minute noise
/// during ordinary use. 10 → one "still alive, no events" line every
/// ~50 s of true idleness, vs. 12/min unconditionally before the cap.
pub(crate) const HEARTBEAT_IDLE_EMIT_EVERY: u64 = 10;
