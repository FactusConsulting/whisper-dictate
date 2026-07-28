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
//!    On healthy sessions the thread retires after
//!    [`HEARTBEAT_HEALTHY_QUOTA`] consecutive event-carrying beats so
//!    an always-on tray install does not accumulate log noise
//!    indefinitely (Codex P2 #646 r3661145603). Zero-event beats keep
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
/// Codex P2 #646 discussion r3661145603.
pub(crate) const HEARTBEAT_HEALTHY_QUOTA: u64 = 720;

/// Fallback emit cadence for healthy heartbeats — during the observation
/// window a beat only writes when `events_since > 0` OR when this many
/// consecutive zero-event beats have elapsed (whichever comes first).
/// Keeps the "hook still up" signal alive on genuinely idle sessions
/// (nobody at the keyboard) while suppressing minute-by-minute noise
/// during ordinary use. 10 → one "still alive, no events" line every
/// ~50 s of true idleness, vs. 12/min unconditionally before the cap.
pub(crate) const HEARTBEAT_IDLE_EMIT_EVERY: u64 = 10;

/// Signals the listener thread sends to the spawn-side coordinator.
enum ListenerSignal {
    /// The thread is up and about to call into rdev.
    Started,
    /// `rdev::listen` returned Err quickly (no display, missing OS
    /// permission, ...). The string is the rdev error formatted for logs.
    Failed(String),
    /// The listener thread could not prime the async diagnostic
    /// writer thread — `crate::diag_async::writer_result` returned
    /// Err. Kept distinct from [`Self::Failed`] so `spawn` can map it
    /// to [`SpawnError::WriterStartup`] and the caller can pick a
    /// diagnostic-degraded fallback strategy rather than a
    /// hotkey-disabled one. Codex P2 #675 PRRT_kwDOSfNjQs6UbAip.
    WriterFailed(String),
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

/// Redact a raw-event name for the diagnostic-log trace line so ordinary
/// desktop typing (letters, digits, punctuation) never lands in
/// `gui-diagnostic.log`. Delegates to the shared
/// [`crate::hotkey::modifier_match::redact_key_name_for_diag`] so the
/// rdev pre-filter line, the tracker's `[chord]` line, and any future
/// hotkey trace all use the same PTT-eligibility predicate — otherwise
/// a fix on one surface leaves the others leaking (Codex P1 PR #665
/// review found the earlier per-surface redaction defeated because the
/// tracker had its own log call using the un-redacted name).
///
/// Codex P1 #646 discussion r3661145597 + Codex P1 #665 discussion
/// PRRT_kwDOSfNjQs6UXh5C.
pub(crate) fn redact_raw_event_name(name: &str) -> &str {
    crate::hotkey::modifier_match::redact_key_name_for_diag(name)
}

/// Redact an `rdev::EventType` for the debug-level pre-filter trace so
/// the `[rdev/callback] raw=…` line does not leak key identity for
/// ordinary desktop typing when `VOICEPI_LOG=debug`/`trace` is on.
///
/// The plain `{:?}` format of `rdev::EventType` prints the raw `Key`
/// variant — `KeyPress(KeyA)`, `KeyPress(Num5)`, `KeyPress(Semicolon)`
/// — for every desktop-wide keydown/keyup. That's exactly the identity
/// the sampled `[hotkey/rdev] raw event #n` line redacts a few lines
/// below; leaving this line unredacted defeats the redaction because
/// **every** event lands here (unsampled), so passwords, tokens and
/// URLs typed anywhere on the desktop can be reconstructed from a
/// debug/trace log window.
///
/// The rule mirrors [`redact_raw_event_name`]: for a key event, if the
/// resolved name is PTT-eligible ([`is_rdev_supported_name`]) keep it
/// (that's the debug-diagnostic signal we care about — an F9 rdev sees
/// but `key_to_name` discards vs. one it maps cleanly); otherwise emit
/// `KeyPress(<redacted>)` / `KeyRelease(<redacted>)`. Non-key events
/// (mouse move, wheel, button) carry no keyboard PII and pass through
/// as their `{:?}` form so mouse-hook interaction stays diagnosable.
///
/// Codex P1 #657 discussion r3663766123.
pub(crate) fn redact_event_type_for_debug(event_type: &rdev::EventType) -> String {
    match event_type {
        rdev::EventType::KeyPress(k) => match key_to_name(*k) {
            Some(name) if is_rdev_supported_name(&name) => format!("KeyPress({name})"),
            _ => "KeyPress(<redacted>)".to_owned(),
        },
        rdev::EventType::KeyRelease(k) => match key_to_name(*k) {
            Some(name) if is_rdev_supported_name(&name) => format!("KeyRelease({name})"),
            _ => "KeyRelease(<redacted>)".to_owned(),
        },
        other => format!("{other:?}"),
    }
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
    // Discard the heartbeat handle — production has never had a use for it.
    // See `spawn_with_raw_tap_capturing_heartbeat_for_tests` for the test-only
    // surface that observes it (Codex P2 #673, thread PRRT_kwDOSfNjQs6UaDcc).
    spawn_with_raw_tap_inner(injection_guard, on_output, raw_tap).0
}

/// Result of the internal spawn helper: the normal spawn outcome plus the
/// heartbeat thread's `JoinHandle` (or `None` if `thread::Builder::spawn`
/// failed to launch it, essentially OOM only). Kept as a type alias to
/// tame `clippy::type_complexity` — the tuple appears on multiple signatures.
type SpawnWithHeartbeatResult = (
    Result<(ManagerHandle, ManagerThread), SpawnError>,
    Option<thread::JoinHandle<()>>,
);

/// Test-only shim that returns both the normal `spawn_with_raw_tap` result
/// AND the heartbeat thread's `JoinHandle`. Callers can then observe that
/// the actual production wiring in `spawn_with_raw_tap_inner`'s error
/// branches (all four `heartbeat_stop.store(true, ...)` calls) genuinely
/// makes the heartbeat thread exit — a property the earlier
/// `spawn_heartbeat_thread_exits_when_stop_is_signalled` test could not
/// pin because it flipped an INDEPENDENT stop atomic instead of exercising
/// the spawn wiring itself. Codex P2 #673 thread PRRT_kwDOSfNjQs6UaDcc.
///
/// The `Option` in the second slot is `None` iff `thread::Builder::spawn`
/// failed to launch the heartbeat thread (essentially OOM only) — tests
/// that require the handle should `expect(...)` it.
#[cfg(test)]
pub(crate) fn spawn_with_raw_tap_capturing_heartbeat_for_tests<F, R>(
    injection_guard: Arc<InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> SpawnWithHeartbeatResult
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    spawn_with_raw_tap_inner(injection_guard, on_output, raw_tap)
}

/// Shared body of `spawn_with_raw_tap` and its test-only companion. Returns
/// the heartbeat `JoinHandle` alongside the usual spawn result so tests can
/// observe that the spawn error-branch wiring actually stops the heartbeat
/// (Codex P2 #673). Production callers ignore the handle.
fn spawn_with_raw_tap_inner<F, R>(
    injection_guard: Arc<InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> SpawnWithHeartbeatResult
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

    // Production ignores the returned `JoinHandle` (the heartbeat runs
    // for process life on shipping installs and the `HotkeyHandle` is
    // never dropped). The handle is threaded up through the outer
    // `(_, Option<JoinHandle>)` return so the test-only companion
    // `spawn_with_raw_tap_capturing_heartbeat_for_tests` can observe the
    // thread actually exits when the spawn error branches store to
    // `heartbeat_stop` — Codex P2 #673 thread PRRT_kwDOSfNjQs6UaDcc.
    // `.ok()` matches the pre-existing "log-and-swallow" behaviour on
    // the essentially-impossible OOM path where the OS refused a thread.
    let heartbeat_handle: Option<thread::JoinHandle<()>> = spawn_heartbeat_thread(
        Arc::clone(&events_total),
        Arc::clone(&events_since_heartbeat),
        Arc::clone(&heartbeat_stop),
    )
    .ok();

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
    let listener_thread = thread::Builder::new()
        .name("vp-hotkey-rdev".to_owned())
        .spawn(move || {
            // Prime the async diagnostic writer BEFORE announcing
            // readiness — the very first `enqueue_or_drop` call
            // would otherwise spawn the writer thread from inside the
            // LL-hook callback, blocking the callback on
            // `Builder::spawn`. Doing it here means the callback path
            // is a single relaxed load + a lock-free channel send.
            // Codex P2 #651 discussion PRRT_kwDOSfNjQs6UTvPm.
            //
            // If the writer spawn itself fails we surface that as a
            // startup error via the ready channel BEFORE sending
            // `Started` — otherwise the manager would announce a
            // successful hotkey installation while every debug/trace
            // record silently disappears for the rest of the process
            // lifetime. Codex P2 #675 PRRT_kwDOSfNjQs6UbAip.
            if let Err(msg) = crate::diag_async::writer_result() {
                let _ = ready_tx.send(ListenerSignal::WriterFailed(msg));
                return;
            }
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
                let debug = crate::diag::debug_enabled();
                // Debug: log every rdev event BEFORE name-filter, so
                // an F9 that rdev sees but key_to_name discards is
                // visible. Complements the parallel WH_KEYBOARD_LL
                // hook (raw hook = OS pump delivered vk; this line =
                // rdev's callback fired with a matching variant).
                //
                // The writes go through the async writer so the
                // file-flush cost does not land on the LL-hook
                // callback — Codex P2 #651 discussion
                // PRRT_kwDOSfNjQs6UTvPm. `crate::diag::log!` here
                // would acquire the diag writer mutex + flush the
                // AppData tee file synchronously, which on a slow
                // volume can exceed Windows' ~300 ms LL-hook
                // timeout and cause the OS to silently uninstall
                // the PTT hook.
                if debug {
                    // Redact key identity but preserve event kind
                    // (Press/Release/mouse variants). Ordinary
                    // desktop typing at `VOICEPI_LOG=debug`/`trace`
                    // would otherwise dump `KeyPress(KeyA)` etc. on
                    // every keystroke, defeating the redaction of
                    // the sampled `[hotkey/rdev] raw event #n` line
                    // just below — Codex P1 #657 discussion
                    // r3663766123.
                    crate::diag_async::enqueue_or_drop(format!(
                        "[rdev/callback] raw={}",
                        redact_event_type_for_debug(&event.event_type)
                    ));
                }
                if let Some(raw) = raw_from_rdev(&event) {
                    // Update counters BEFORE the guard / tracker check —
                    // the heartbeat records every raw event rdev delivered,
                    // even ones self-injection filtering drops. If the
                    // guard is armed and the event is swallowed the raw
                    // count still moves; if the pump is dead the count
                    // stays flat. That's the exact signal we need.
                    let n = listener_total.fetch_add(1, Ordering::Relaxed) + 1;
                    listener_since.fetch_add(1, Ordering::Relaxed);
                    if crate::diag::info_enabled() && should_log_raw_event(n) {
                        // Redact non-PTT-eligible names so ordinary desktop
                        // typing (passwords/tokens/URLs) doesn't get sampled
                        // into gui-diagnostic.log — Codex P1 #646 r3661145597.
                        // `kind` (Press / Release) stays for the wedge-signal.
                        // Off-loaded onto the async writer so a slow file
                        // flush cannot stall the LL-hook callback — Codex
                        // P2 #651 discussion PRRT_kwDOSfNjQs6UTvPm.
                        crate::diag_async::enqueue_or_drop(format!(
                            "[hotkey/rdev] raw event #{n}: name={:?} kind={:?}",
                            redact_raw_event_name(&raw.name),
                            raw.kind
                        ));
                    }
                    // Debug: post-name-filter — the value the tracker
                    // actually keys off. Mismatch with the raw= line
                    // above pinpoints a bug in key_to_name. Redact
                    // non-PTT names to keep passwords/tokens out of
                    // debug/trace uploads (Codex P1 #646 r3661145597).
                    if debug {
                        crate::diag_async::enqueue_or_drop(format!(
                            "[rdev/callback] mapped_name={:?} kind={:?}",
                            redact_raw_event_name(&raw.name),
                            raw.kind
                        ));
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
                    // Pass the driver label so `dispatch_raw_event`'s
                    // guard-drop diagnostic emits a backend-neutral
                    // marker instead of always attributing the drop to
                    // rdev — Codex P2 #675 PRRT_kwDOSfNjQs6UbAiZ.
                    if let Some(out) = dispatch_raw_event(&listener_guard, &mut t, &raw, "rdev") {
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
        });
    if let Err(e) = listener_thread {
        // Codex P2 #646 r3661145600: the heartbeat thread is already
        // running by the time we get here; if the listener never spawns
        // it would write `listener heartbeat; events_since=0` every 5 s
        // forever, misleadingly signalling a hung LL-hook. Stop the
        // heartbeat before returning so a caller that retries doesn't
        // stack orphans either.
        heartbeat_stop.store(true, Ordering::Relaxed);
        return (
            Err(SpawnError::ListenerStartup(format!(
                "thread spawn failed: {e}"
            ))),
            heartbeat_handle,
        );
    }

    // Wait for the listener thread to report it's up. Without this we'd
    // race the manager thread's spawn against the OS scheduler. On every
    // early-return path we STOP the heartbeat first, so a caller that
    // retries after a startup failure does not accumulate orphan
    // heartbeat threads (Codex P2 #646 r3661145600).
    match ready_rx.recv_timeout(READY_PROBE_WINDOW) {
        Ok(ListenerSignal::Started) => {}
        Ok(ListenerSignal::Failed(msg)) => {
            heartbeat_stop.store(true, Ordering::Relaxed);
            return (Err(SpawnError::ListenerStartup(msg)), heartbeat_handle);
        }
        Ok(ListenerSignal::WriterFailed(msg)) => {
            // Diagnostic writer never came up — the listener returned
            // BEFORE calling `rdev::listen`, so there is no live OS
            // hook to clean up. Stop the heartbeat and surface the
            // distinct error so the supervisor can log + degrade.
            // Codex P2 #675 PRRT_kwDOSfNjQs6UbAip.
            heartbeat_stop.store(true, Ordering::Relaxed);
            return Err(SpawnError::WriterStartup(msg));
        }
        Err(_) => {
            heartbeat_stop.store(true, Ordering::Relaxed);
            return (Err(SpawnError::ListenerHung), heartbeat_handle);
        }
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
            Ok(ListenerSignal::Failed(msg)) => {
                heartbeat_stop.store(true, Ordering::Relaxed);
                return (Err(SpawnError::ListenerStartup(msg)), heartbeat_handle);
            }
            Ok(ListenerSignal::WriterFailed(msg)) => {
                heartbeat_stop.store(true, Ordering::Relaxed);
                return Err(SpawnError::WriterStartup(msg));
            }
            Ok(ListenerSignal::Started) => {} // duplicate, ignore
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break, // listener exited without err — healthy
        }
    }

    let manager_thread = match spawn_manager_thread(cmd_rx, Arc::clone(&tracker)) {
        Ok(t) => t,
        Err(err) => {
            // Same rationale as the listener-thread spawn failure branch
            // above: the heartbeat is already running, so stop it before
            // returning or a retry-happy caller stacks orphans.
            heartbeat_stop.store(true, Ordering::Relaxed);
            return (Err(err), heartbeat_handle);
        }
    };
    (Ok((handle, manager_thread)), heartbeat_handle)
}

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
/// heartbeat-stop lifecycle fix (Codex P2 #657 r3663766095).
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
/// Codex P2 #673 thread PRRT_kwDOSfNjQs6UaDch — the earlier
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
                             beat to bound gui-diagnostic.log growth on long-running tray installs \
                             (Codex P2 #646 r3661145603). A future wedge will no longer be visible \
                             via heartbeat lines; rely on the tracker/coordinator diagnostics.",
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
/// Codex P2 #646 r3661145603.
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
    /// production window (Codex P2 #673 thread PRRT_kwDOSfNjQs6UaDch).
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
    /// retirement branch on a millisecond timescale — Codex P2 #673
    /// thread PRRT_kwDOSfNjQs6UaDch.
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
    // `alt_r` is a valid side-specific binding: `modifier_family` classifies
    // it as `alt`, `canonical_side("alt_gr") == canonical_side("alt_r")`, and
    // an OS-delivered `alt_gr` / `right_alt` press satisfies the binding.
    // Without it in this list, install-time validation rejected `alt_r+...`
    // chords the moment RegisterHotKey rejected them for being side-specific
    // (Codex P2 #656 r3663653258, PR #650 fallback path).
    "alt_r",
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
    // Windows-terminology aliases for the Meta / Super key family
    // (rdev emits `cmd_l` / `cmd_r`). `modifier_family` / `canonical_side`
    // treat these as `cmd`-family equivalents so a `win_l+f9` binding
    // (rejected by RegisterHotKey as side-specific, PR #650) reaches the
    // rdev fallback and matches real Meta-key presses. Codex P2 #656
    // r3663653258.
    "win",
    "win_l",
    "win_r",
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
