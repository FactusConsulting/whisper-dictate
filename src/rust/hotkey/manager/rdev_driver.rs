//! `rdev` driver layer — only compiled when the `rust-hotkeys` feature is on.
//!
//! Two threads per subsystem:
//!
//! * the *listener* thread, which calls `rdev::listen` and blocks forever
//!   (rdev has no clean stop API), translating each `rdev::Event` into a
//!   [`RawKeyEvent`] and feeding the shared [`KeyTracker`];
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
//! caller of `install_hotkey()` so the supervisor can keep the Python
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
//! 2. A **rate-limited per-event trace**: the first
//!    [`RAW_EVENT_INITIAL_TRACE`] events always log, then every
//!    [`RAW_EVENT_TRACE_EVERY`]th event. This surfaces the actual key
//!    names rdev delivers so a chord-matcher rejection ("hook is alive
//!    but the event is called `ctrl` not `ctrl_l`", or "AltGr shows up
//!    as `alt_gr` but the user configured `alt_r`") is visible without
//!    a rebuild.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::driver_common::{manager_channel, spawn_manager_thread};
use super::tracker::{KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput};
use crate::hotkey::inject_guard::{dispatch_raw_event, InjectionGuard};

pub use super::driver_common::{
    ManagerCommand, ManagerHandle, ManagerThread, NoopRawTap, RawTap, SpawnError,
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

/// Signals the listener thread sends to the spawn-side coordinator.
enum ListenerSignal {
    /// The thread is up and about to call into rdev.
    Started,
    /// `rdev::listen` returned Err quickly (no display, missing OS
    /// permission, ...). The string is the rdev error formatted for logs.
    Failed(String),
}

/// Rate-limit decision for the per-event trace line. Pure so it can be
/// unit-tested without spawning any threads — the runtime just calls it
/// with the current event counter (1-indexed).
///
/// Returns true for events `1..=RAW_EVENT_INITIAL_TRACE`, then for every
/// `RAW_EVENT_TRACE_EVERY`-th event thereafter. Zero is treated as "no
/// index" and always returns false; callers must pass at least 1.
pub(crate) fn should_log_raw_event(n: u64) -> bool {
    if n == 0 {
        return false;
    }
    if n <= RAW_EVENT_INITIAL_TRACE {
        return true;
    }
    n.is_multiple_of(RAW_EVENT_TRACE_EVERY)
}

/// Spawn the manager thread plus the `rdev` listener thread. Every tracker
/// output produced by a real OS key event is dispatched to `on_output`,
/// which the coordinator hooks up to its press/release/cancel events.
///
/// Returns `Err(SpawnError)` if the rdev listener fails to start within
/// [`READY_PROBE_WINDOW`] — for example missing X display on Linux, or
/// missing accessibility permission on macOS. On success the listener thread
/// runs forever (rdev limitation) and is reported as healthy.
///
/// `injection_guard` gates the callback: while the injector wrapper is
/// bursting synthetic events through `SendInput` (Windows) / equivalent
/// APIs on X11+macOS, the guard is armed and the callback drops every
/// event rdev delivers. This closes the Windows self-injection PTT
/// wedge — the same class of bug the Wayland fix in #467 solved via
/// device-level filtering. See [`crate::hotkey::inject_guard`] for the
/// full rationale and timing model.
#[cfg(test)]
pub fn spawn<F>(
    injection_guard: Arc<InjectionGuard>,
    on_output: F,
) -> Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    spawn_with_raw_tap(injection_guard, on_output, NoopRawTap)
}

/// Same as [`spawn`] but also invokes `raw_tap` for every raw OS key event
/// BEFORE the tracker sees it. The tap runs on the rdev listener thread —
/// keep it cheap and non-blocking (long work will delay the tracker and
/// starve the coordinator).
///
/// The raw tap DOES run for self-injected events too (before the
/// [`InjectionGuard`] check), so the diagnostic `hotkey capture` CLI
/// can still surface the injector's own SendInput bursts. Only the
/// tracker (and therefore the coordinator's chord state) is protected
/// from the feedback loop.
pub fn spawn_with_raw_tap<F, R>(
    injection_guard: Arc<InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    let (handle, cmd_rx) = manager_channel();
    let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));
    let on_output = Arc::new(on_output);
    let raw_tap = Arc::new(raw_tap);

    // Per-listener event counters — updated on every raw OS event from
    // inside the LL-hook callback, read by the heartbeat thread. Atomics
    // (rather than `Mutex<u64>`) so the callback stays lock-free on the
    // hot path — matters because on Windows the callback runs from
    // inside the LL-hook thread for every desktop-wide keydown/keyup.
    let events_total = Arc::new(AtomicU64::new(0));
    let events_since_heartbeat = Arc::new(AtomicU64::new(0));
    // Heartbeat stop signal — set on Drop of the listener side would be
    // ideal, but the rdev listener thread cannot be joined, and the
    // production `HotkeyHandle` is a process-lifetime resource (never
    // dropped in shipping code). The heartbeat thread therefore runs
    // until process exit; the atomic exists so the test-only shutdown
    // path can nudge it, and so a future rework can wire it in without
    // reshaping the spawn signature.
    let heartbeat_stop = Arc::new(AtomicBool::new(false));

    spawn_heartbeat_thread(
        Arc::clone(&events_total),
        Arc::clone(&events_since_heartbeat),
        Arc::clone(&heartbeat_stop),
    );

    // Listener thread — owns rdev. Translates raw events through the shared
    // tracker. Signals readiness / startup failure on a sync channel so
    // `spawn` can surface a quick-failure to the caller (P1 finding #2).
    let listener_tracker = Arc::clone(&tracker);
    let listener_sink = Arc::clone(&on_output);
    let listener_tap = Arc::clone(&raw_tap);
    let listener_guard = Arc::clone(&injection_guard);
    let listener_total = Arc::clone(&events_total);
    let listener_since = Arc::clone(&events_since_heartbeat);
    let (ready_tx, ready_rx) = mpsc::channel::<ListenerSignal>();
    thread::Builder::new()
        .name("vp-hotkey-rdev".to_owned())
        .spawn(move || {
            // Announce we're up BEFORE blocking in rdev::listen — without
            // this the spawn-side can't tell "thread never scheduled" apart
            // from "rdev is blocking healthily".
            let _ = ready_tx.send(ListenerSignal::Started);
            // Diagnostic marker so the tee file records the listener
            // thread actually reached rdev::listen. Combined with the
            // heartbeat, "startup log line present + heartbeat present
            // but events_since_last_heartbeat=0" is the Windows PTT
            // wedge signature (hook installed, pump idle).
            crate::diag::log!(
                "[hotkey/rdev] listener thread started; installing global hook \
                 (WH_KEYBOARD_LL on Windows / XRecord on X11 / CGEventTap on macOS)"
            );
            let cb = move |event: rdev::Event| {
                if let Some(raw) = raw_from_rdev(&event) {
                    // Update counters BEFORE the guard / tracker check —
                    // the heartbeat records every raw event rdev delivered,
                    // even ones self-injection filtering drops. If the
                    // guard is armed and the event is swallowed the raw
                    // count still moves; if the pump is dead the count
                    // stays flat. That's the exact signal we need.
                    let n = listener_total.fetch_add(1, Ordering::Relaxed) + 1;
                    listener_since.fetch_add(1, Ordering::Relaxed);
                    if should_log_raw_event(n) {
                        crate::diag::log!(
                            "[hotkey/rdev] raw event #{n}: name={:?} kind={:?}",
                            raw.name,
                            raw.kind
                        );
                    }
                    listener_tap.tap(&raw);
                    // `dispatch_raw_event` short-circuits when the guard
                    // is armed — the injector's own SendInput bursts get
                    // dropped here instead of feeding back into the
                    // tracker (Windows PTT wedge; module doc explains
                    // why the check must live below the mutex acquire so
                    // event ordering is preserved). Fast path when the
                    // guard is inactive is two atomic loads + a compare
                    // and does NOT allocate — matters because this
                    // callback runs on the LL-hook thread for every
                    // desktop-wide keydown/keyup (PR #478 regression).
                    let mut t = listener_tracker.lock().expect("tracker poisoned");
                    if let Some(out) = dispatch_raw_event(&listener_guard, &mut t, &raw) {
                        (listener_sink)(out);
                    }
                }
            };
            if let Err(err) = rdev::listen(cb) {
                let msg = format!("{err:?}");
                // Tee via `crate::diag::log!` so the failure surfaces in
                // the Windows GUI diagnostic file (`gui-diagnostic.log`)
                // — plain `eprintln!` here would be discarded because
                // `whisper-dictate-gui.exe` is `windows_subsystem =
                // "windows"` and has no console attached (the Windows
                // PTT bug-report symptom that showed up as "Stderr is
                // silent (0 bytes) even with RUST_LOG=debug").
                crate::diag::log!("[hotkey] rdev listener failed: {msg}");
                let _ = ready_tx.send(ListenerSignal::Failed(msg));
            } else {
                // rdev's `listen` is documented to block for the process
                // lifetime on success. If it EVER returns Ok, the LL
                // hook the listener installed against this thread is
                // now dead (per-thread lifetime), and PTT will silently
                // fail for the rest of the process. Surface it via the
                // diagnostic channel so a future wedge that traces back
                // to this early exit is inspectable in the Windows GUI
                // diagnostic log.
                crate::diag::log!(
                    "[hotkey] rdev listener returned Ok - the OS \
                     message loop exited before shutdown. PTT will no \
                     longer fire on this process; the hook the listener \
                     installed against this thread is uninstalled with \
                     the thread. This is unexpected on healthy sessions."
                );
            }
        })
        .map_err(|e| SpawnError::ListenerStartup(format!("thread spawn failed: {e}")))?;

    // Wait for the listener thread to report it's up. Without this we'd
    // race the manager thread's spawn against the OS scheduler.
    match ready_rx.recv_timeout(READY_PROBE_WINDOW) {
        Ok(ListenerSignal::Started) => {}
        Ok(ListenerSignal::Failed(msg)) => return Err(SpawnError::ListenerStartup(msg)),
        Err(_) => return Err(SpawnError::ListenerHung),
    }
    // Give rdev a short window to fail fast. On platforms where listen()
    // returns Err (no display, missing permissions) it does so very early
    // in the call; if no error arrives within READY_PROBE_WINDOW we assume
    // it's blocking healthily.
    let deadline = Instant::now() + READY_PROBE_WINDOW;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match ready_rx.recv_timeout(remaining) {
            Ok(ListenerSignal::Failed(msg)) => return Err(SpawnError::ListenerStartup(msg)),
            Ok(ListenerSignal::Started) => {} // duplicate, ignore
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break, // listener exited without err — healthy
        }
    }

    let manager_thread = spawn_manager_thread(cmd_rx, Arc::clone(&tracker))?;
    Ok((handle, manager_thread))
}

/// Spawn the heartbeat thread. Runs until `stop` flips to `true`, which in
/// production never happens — the rdev listener itself cannot be joined so
/// there is no clean shutdown for the diagnostic layer above it either.
/// Test hosts flip the atomic to keep the tee file from growing across a
/// full unit-test run; production callers ignore it and accept a
/// process-lifetime thread.
///
/// The thread name is set explicitly so a Windows dump / a `taskkill /f
/// /t` trace names it, and so `Thread::current().name()` in a future
/// panic hook can attribute stack traces to the right subsystem.
fn spawn_heartbeat_thread(
    events_total: Arc<AtomicU64>,
    events_since_heartbeat: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) {
    let _ = thread::Builder::new()
        .name("vp-hotkey-rdev-heartbeat".to_owned())
        .spawn(move || {
            // First heartbeat also emits an install-time marker so the
            // absolute t=<ms> value of the first `heartbeat` line pins
            // the listener-start moment even if the LL hook is silent.
            crate::diag::log!(
                "[hotkey/rdev] heartbeat thread started; interval={:?}",
                HEARTBEAT_INTERVAL
            );
            while !stop.load(Ordering::Relaxed) {
                // Sleep in small slices so the stop signal is honoured
                // promptly in tests. A production process never toggles
                // stop, so the sliced sleep is a no-op there.
                let deadline = Instant::now() + HEARTBEAT_INTERVAL;
                while Instant::now() < deadline {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let remaining = deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(250));
                    thread::sleep(remaining);
                }
                let total = events_total.load(Ordering::Relaxed);
                let since = events_since_heartbeat.swap(0, Ordering::Relaxed);
                crate::diag::log!(
                    "[hotkey/rdev] listener heartbeat; events_since_last_heartbeat={since}; \
                     total_events={total}"
                );
            }
        });
}

/// Convert an `rdev::Event` into the platform-agnostic [`RawKeyEvent`] the
/// tracker consumes. Returns `None` only for non-keyboard events (mouse,
/// etc.); unknown key variants get a synthetic `__rdev_<Debug>` name so the
/// tracker can still detect foreign-key holds for bare-modifier rule 1/2
/// (P2 #346 finding 2). PTT-target matching never collides with these names
/// since every PTT-able name is in `key_to_name`.
///
/// `pub(crate)` so the companion `rdev_driver_tests.rs` can drive it with
/// synthetic events without a shim.
pub(crate) fn raw_from_rdev(event: &rdev::Event) -> Option<RawKeyEvent> {
    let (key, kind) = match event.event_type {
        rdev::EventType::KeyPress(k) => (k, RawKeyKind::Press),
        rdev::EventType::KeyRelease(k) => (k, RawKeyKind::Release),
        _ => return None,
    };
    let name = key_to_name(key).unwrap_or_else(|| format!("__rdev_{key:?}"));
    Some(RawKeyEvent {
        name,
        kind,
        at: Instant::now(),
    })
}

/// Names the rdev driver can actually translate into [`RawKeyEvent`]s. A
/// PTT binding whose name isn't in this set silently never fires — see
/// [`is_rdev_supported_name`] for the install-time validator that rejects
/// such bindings up front (P2 finding #6).
const RDEV_SUPPORTED_NAMES: &[&str] = &[
    "ctrl_l",
    "ctrl_r",
    "ctrl",
    "shift_l",
    "shift_r",
    "shift",
    "alt_l",
    "alt",
    "alt_gr",
    // `right_alt` and `ralt` are accepted aliases for `alt_gr` / AltGr
    // (P2 #346 finding 4): rdev maps both to K::AltGr → "alt_gr" via
    // `key_to_name`, and `modifier_family` / `canonical_side` treat them
    // as equivalent to `alt_r`, so the tracker matches correctly.
    "right_alt",
    "ralt",
    "cmd_l",
    "cmd_r",
    "cmd",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "space",
    "esc",
    "tab",
    "enter",
];

/// True if `name` is one of the PTT-binding names the rdev driver can
/// translate. Used by the hotkey installer to reject (or remap) Python-only
/// names like `super_l` / `super_r` before the supervisor disables the
/// Python listener. The generic `ctrl` / `shift` / `alt` / `cmd` are
/// included because they are valid PTT *bindings* even though rdev never
/// emits them as raw events — `modifier_matches` handles the matching, and
/// rule 1 / 2 still need to know they're targets.
pub fn is_rdev_supported_name(name: &str) -> bool {
    RDEV_SUPPORTED_NAMES.contains(&name)
}

/// Map `rdev::Key` to the lowercase-name convention used by the Python PTT
/// settings (`ctrl_l`, `shift_r`, `alt_gr`, `f9`, single chars, ...).
/// Unmapped keys return `None` — they cannot be a PTT target so silently
/// dropping them is fine.
fn key_to_name(key: rdev::Key) -> Option<String> {
    use rdev::Key as K;
    let name = match key {
        K::ControlLeft => "ctrl_l",
        K::ControlRight => "ctrl_r",
        K::ShiftLeft => "shift_l",
        K::ShiftRight => "shift_r",
        K::Alt => "alt_l",
        K::AltGr => "alt_gr",
        K::MetaLeft => "cmd_l",
        K::MetaRight => "cmd_r",
        K::F1 => "f1",
        K::F2 => "f2",
        K::F3 => "f3",
        K::F4 => "f4",
        K::F5 => "f5",
        K::F6 => "f6",
        K::F7 => "f7",
        K::F8 => "f8",
        K::F9 => "f9",
        K::F10 => "f10",
        K::F11 => "f11",
        K::F12 => "f12",
        K::Space => "space",
        K::Escape => "esc",
        K::Tab => "tab",
        K::Return => "enter",
        _ => return None,
    };
    Some(name.to_owned())
}

// Unit tests moved to sibling `rdev_driver_tests.rs` so the regression
// -test discipline scanner sees a matching companion file for the new
// `should_log_raw_event` helper and the heartbeat instrumentation.
