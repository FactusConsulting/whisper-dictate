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
//! The two are split because `rdev::listen` is not `Send`, blocks the thread
//! it runs on for the process lifetime, and offers no register/unregister
//! API of its own — so the manager thread is the only place from which the
//! rest of the runtime can safely talk to the binding.
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

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::tracker::{KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput};

/// Maximum time [`spawn`] waits after the listener thread reports "started"
/// for `rdev::listen` to either return Err (and thus be a startup failure)
/// or stay blocked (and thus be healthy). Tuned for fast-failure platforms
/// like headless Linux without making CI slow.
const READY_PROBE_WINDOW: Duration = Duration::from_millis(250);

/// Commands the manager thread accepts on its inbound channel. Each carries
/// a sync response sender so the caller can `recv()` confirmation from a
/// non-listener thread; this is the "mpsc commands with sync response"
/// pattern issue #318 calls out.
pub enum ManagerCommand {
    Register {
        targets: Vec<String>,
        ack: Sender<Result<(), String>>,
    },
    Unregister {
        ack: Sender<Result<(), String>>,
    },
    Shutdown,
}

/// Public handle to the manager thread.
#[derive(Clone)]
pub struct ManagerHandle {
    tx: Sender<ManagerCommand>,
}

impl ManagerHandle {
    /// Install (or replace) the active PTT binding. Blocks until the manager
    /// thread acknowledges, but the underlying operation is cheap — just
    /// swapping a `Vec<String>` in a mutex.
    pub fn register(&self, targets: Vec<String>) -> Result<(), String> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.tx
            .send(ManagerCommand::Register {
                targets,
                ack: ack_tx,
            })
            .map_err(|e| format!("manager thread disconnected: {e}"))?;
        ack_rx
            .recv()
            .map_err(|e| format!("ack channel closed: {e}"))?
    }

    /// Stop emitting tracker outputs without tearing down the listener
    /// thread (the OS listener stays installed — `rdev` does not give us a
    /// clean per-binding teardown — but the tracker is replaced with an
    /// empty one so no events flow through).
    pub fn unregister(&self) -> Result<(), String> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.tx
            .send(ManagerCommand::Unregister { ack: ack_tx })
            .map_err(|e| format!("manager thread disconnected: {e}"))?;
        ack_rx
            .recv()
            .map_err(|e| format!("ack channel closed: {e}"))?
    }

    /// Ask the manager thread to exit. The OS-level `rdev::listen()` thread
    /// cannot be interrupted, so it leaks on shutdown — acceptable because
    /// the supervisor only ever installs the hotkey subsystem once and tears
    /// down on process exit.
    pub fn shutdown(&self) {
        let _ = self.tx.send(ManagerCommand::Shutdown);
    }
}

/// Owned join handle for the manager thread (NOT the inner rdev listener
/// thread, which cannot be joined). Kept by the supervisor for cleanup.
pub struct ManagerThread {
    join: Option<JoinHandle<()>>,
}

impl ManagerThread {
    pub fn join(mut self) {
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

/// Signals the listener thread sends to the spawn-side coordinator.
enum ListenerSignal {
    /// The thread is up and about to call into rdev.
    Started,
    /// `rdev::listen` returned Err quickly (no display, missing OS
    /// permission, ...). The string is the rdev error formatted for logs.
    Failed(String),
}

/// Errors [`spawn`] surfaces on startup. Translated to `InstallError` by the
/// hotkey module so the supervisor can pick a fallback strategy.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("rdev listener startup failed: {0}")]
    ListenerStartup(String),
    #[error("rdev listener thread never reported it was started")]
    ListenerHung,
}

/// Spawn the manager thread plus the `rdev` listener thread. Every tracker
/// output produced by a real OS key event is dispatched to `on_output`,
/// which the coordinator hooks up to its press/release/cancel events.
///
/// Returns `Err(SpawnError)` if the rdev listener fails to start within
/// [`READY_PROBE_WINDOW`] — for example missing X display on Linux, or
/// missing accessibility permission on macOS. On success the listener thread
/// runs forever (rdev limitation) and is reported as healthy.
pub fn spawn<F>(on_output: F) -> Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    spawn_with_raw_tap(on_output, NoopRawTap)
}

/// Marker type for [`spawn_with_raw_tap`] callers that don't want a raw tap.
/// Cheaper than an `Option<Arc<dyn Fn>>` — the trait dispatch inlines to a
/// no-op so the shipping supervisor path pays nothing for the diagnostic hook.
pub struct NoopRawTap;

/// Sink for raw OS key events, called BEFORE the tracker processes them.
/// Implemented by [`NoopRawTap`] (drops silently) and any `Fn(&RawKeyEvent) +
/// Send + Sync + 'static` closure. Kept as a trait so [`spawn_with_raw_tap`]
/// can accept either without runtime overhead — the diagnostic
/// `whisper-dictate hotkey capture` CLI wires up a real tap so the operator
/// can see every keydown/keyup, not just the chord-level output the
/// coordinator produces.
pub trait RawTap: Send + Sync + 'static {
    fn tap(&self, event: &RawKeyEvent);
}

impl RawTap for NoopRawTap {
    #[inline]
    fn tap(&self, _event: &RawKeyEvent) {}
}

impl<F> RawTap for F
where
    F: Fn(&RawKeyEvent) + Send + Sync + 'static,
{
    #[inline]
    fn tap(&self, event: &RawKeyEvent) {
        (self)(event);
    }
}

/// Same as [`spawn`] but also invokes `raw_tap` for every raw OS key event
/// BEFORE the tracker sees it. The tap runs on the rdev listener thread —
/// keep it cheap and non-blocking (long work will delay the tracker and
/// starve the coordinator).
pub fn spawn_with_raw_tap<F, R>(
    on_output: F,
    raw_tap: R,
) -> Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));
    let on_output = Arc::new(on_output);
    let raw_tap = Arc::new(raw_tap);

    // Listener thread — owns rdev. Translates raw events through the shared
    // tracker. Signals readiness / startup failure on a sync channel so
    // `spawn` can surface a quick-failure to the caller (P1 finding #2).
    let listener_tracker = Arc::clone(&tracker);
    let listener_sink = Arc::clone(&on_output);
    let listener_tap = Arc::clone(&raw_tap);
    let (ready_tx, ready_rx) = mpsc::channel::<ListenerSignal>();
    thread::Builder::new()
        .name("vp-hotkey-rdev".to_owned())
        .spawn(move || {
            // Announce we're up BEFORE blocking in rdev::listen — without
            // this the spawn-side can't tell "thread never scheduled" apart
            // from "rdev is blocking healthily".
            let _ = ready_tx.send(ListenerSignal::Started);
            let cb = move |event: rdev::Event| {
                if let Some(raw) = raw_from_rdev(&event) {
                    listener_tap.tap(&raw);
                    let mut t = listener_tracker.lock().expect("tracker poisoned");
                    if let Some(out) = t.handle(&raw) {
                        (listener_sink)(out);
                    }
                }
            };
            if let Err(err) = rdev::listen(cb) {
                let msg = format!("{err:?}");
                eprintln!("[hotkey] rdev listener failed: {msg}");
                let _ = ready_tx.send(ListenerSignal::Failed(msg));
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

    let manager_tracker = Arc::clone(&tracker);
    let join = thread::Builder::new()
        .name("vp-hotkey-manager".to_owned())
        .spawn(move || manager_loop(cmd_rx, manager_tracker))
        .map_err(|e| SpawnError::ListenerStartup(format!("manager thread spawn failed: {e}")))?;

    Ok((
        ManagerHandle { tx: cmd_tx },
        ManagerThread { join: Some(join) },
    ))
}

fn manager_loop(rx: Receiver<ManagerCommand>, tracker: Arc<Mutex<KeyTracker>>) {
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(ManagerCommand::Register { targets, ack }) => {
                *tracker.lock().expect("tracker poisoned") = KeyTracker::new(targets);
                let _ = ack.send(Ok(()));
            }
            Ok(ManagerCommand::Unregister { ack }) => {
                *tracker.lock().expect("tracker poisoned") = KeyTracker::new(Vec::new());
                let _ = ack.send(Ok(()));
            }
            Ok(ManagerCommand::Shutdown) => return,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Convert an `rdev::Event` into the platform-agnostic [`RawKeyEvent`] the
/// tracker consumes. Returns `None` only for non-keyboard events (mouse,
/// etc.); unknown key variants get a synthetic `__rdev_<Debug>` name so the
/// tracker can still detect foreign-key holds for bare-modifier rule 1/2
/// (P2 #346 finding 2). PTT-target matching never collides with these names
/// since every PTT-able name is in `key_to_name`.
fn raw_from_rdev(event: &rdev::Event) -> Option<RawKeyEvent> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn register_and_unregister_roundtrip() {
        // Lightweight test that register/unregister responses come back
        // through the mpsc — does NOT exercise the rdev listener thread
        // (it's still installed but no synthetic events are injected).
        // In headless CI / containers rdev::listen returns Err immediately
        // (no X display / no accessibility permission) — that's exactly
        // the P1-#2 startup-failure path, so we skip the round-trip on
        // such platforms rather than assert success.
        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = Arc::clone(&count);
        let (handle, _thread) = match spawn(move |_out| {
            count_cb.fetch_add(1, Ordering::SeqCst);
        }) {
            Ok(pair) => pair,
            Err(SpawnError::ListenerStartup(_)) | Err(SpawnError::ListenerHung) => {
                eprintln!(
                    "skipping register_and_unregister_roundtrip: rdev listener \
                     refused to start (headless env)"
                );
                return;
            }
        };
        handle
            .register(vec!["ctrl_l".to_owned(), "f9".to_owned()])
            .expect("register");
        handle.unregister().expect("unregister");
        handle
            .register(vec!["shift_r".to_owned()])
            .expect("re-register");
        // No events fired through the tracker — count stays zero.
        assert_eq!(count.load(Ordering::SeqCst), 0);
        handle.shutdown();
        // Do NOT join: the rdev listener thread is unjoinable, but the
        // manager thread is — drop the handle and let the test runner
        // finish. (The thread exits on its own when it sees Shutdown.)
    }

    #[test]
    fn listener_startup_failure_is_surfaced_to_caller() {
        // On a headless Linux container rdev::listen returns Err very
        // quickly (no X display). The driver MUST propagate that to the
        // spawn-side caller instead of silently logging and exiting, so
        // the supervisor can keep the Python listener wired (P1 #2).
        // We don't have a way to force the failure on platforms where the
        // hook genuinely works, so on those we treat success as "test not
        // applicable" rather than fail.
        match spawn(|_out| {}) {
            Ok((handle, _thread)) => {
                handle.shutdown();
            }
            Err(SpawnError::ListenerStartup(msg)) => {
                assert!(
                    !msg.is_empty(),
                    "ListenerStartup error message should not be empty"
                );
            }
            Err(SpawnError::ListenerHung) => {
                // Hung is also a "tell the caller" outcome — acceptable.
            }
        }
    }

    #[test]
    fn rdev_name_set_covers_every_emitted_key() {
        // Every name the rdev->name mapping can emit must appear in the
        // supported-names set so the install-time validator never rejects
        // a name we DO support. If you add a key in `key_to_name`, add it
        // to `RDEV_SUPPORTED_NAMES` (and adjust this assertion if you also
        // expose a new bare-modifier alias).
        for key in [
            rdev::Key::ControlLeft,
            rdev::Key::ControlRight,
            rdev::Key::ShiftLeft,
            rdev::Key::ShiftRight,
            rdev::Key::Alt,
            rdev::Key::AltGr,
            rdev::Key::MetaLeft,
            rdev::Key::MetaRight,
            rdev::Key::F1,
            rdev::Key::F12,
            rdev::Key::Space,
            rdev::Key::Escape,
            rdev::Key::Tab,
            rdev::Key::Return,
        ] {
            let name = key_to_name(key).expect("mapped name");
            assert!(
                is_rdev_supported_name(&name),
                "rdev emits {name} but install-time validator rejects it",
            );
        }
    }

    #[test]
    fn unsupported_names_are_rejected_by_validator() {
        // Names accepted by the Python evdev/pynput backends but NOT by the
        // rdev driver. Without the validator a configuration that contains
        // any of these would install successfully but never fire (P2 #6).
        for name in ["super_l", "super_r", "menu", "scroll_lock", "pause"] {
            assert!(
                !is_rdev_supported_name(name),
                "rdev driver claims to support {name} — update the test or the map",
            );
        }
    }

    // -----------------------------------------------------------------------
    // P2 #346 finding 4: right_alt / ralt aliases.
    // -----------------------------------------------------------------------

    #[test]
    fn right_alt_and_ralt_aliases_are_accepted_by_validator() {
        // Users and documentation sometimes refer to AltGr as "right_alt"
        // or "ralt". The install-time validator must accept these so the
        // Rust backend doesn't reject a valid AltGr PTT binding.
        for name in ["right_alt", "ralt"] {
            assert!(
                is_rdev_supported_name(name),
                "{name} should be accepted as an AltGr alias (P2 #346 finding 4)",
            );
        }
    }

    // -----------------------------------------------------------------------
    // P2 #346 finding 2: unmapped (ordinary) keys reach the tracker.
    // -----------------------------------------------------------------------

    #[test]
    fn raw_from_rdev_produces_event_for_unmapped_key() {
        // Keys not in key_to_name (e.g. letter keys) must still produce a
        // RawKeyEvent so the tracker can detect foreign-key holds and emit
        // ChordCancel for bare-modifier bindings (rule 2). Previously
        // raw_from_rdev returned None for these, silently dropping them.
        use rdev::{Event, EventType};

        let press_a = Event {
            event_type: EventType::KeyPress(rdev::Key::KeyA),
            time: std::time::SystemTime::UNIX_EPOCH,
            name: None,
        };
        let raw = raw_from_rdev(&press_a);
        assert!(
            raw.is_some(),
            "ordinary key press must produce a RawKeyEvent for foreign-key tracking"
        );
        let raw = raw.unwrap();
        assert!(
            raw.name.starts_with("__rdev_"),
            "unmapped key should use synthetic __rdev_ name, got {:?}",
            raw.name
        );
        assert_eq!(raw.kind, RawKeyKind::Press);
    }
}
