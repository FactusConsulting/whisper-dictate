//! Driver-agnostic plumbing shared by every OS-listener backend
//! ([`super::rdev_driver`] on X11 / Windows / macOS, [`super::evdev_driver`]
//! on Linux/Wayland).
//!
//! A "driver" has two halves:
//!
//! * a **listener** half that owns the platform global-key hook, blocks on it
//!   for the process lifetime, and pushes each translated [`crate::hotkey::manager::RawKeyEvent`]
//!   through a shared `Mutex<KeyTracker>` into an `on_output` sink — this half
//!   is platform-specific and lives in each driver module; and
//! * a **manager** half — a thread that owns the same `Mutex<KeyTracker>` and
//!   services `register` / `unregister` / `shutdown` commands over an mpsc so
//!   the rest of the runtime (which is not on the listener thread and cannot
//!   touch the non-`Send` native handle) can swap the active PTT binding.
//!
//! The manager half is byte-identical across backends, so it lives here along
//! with the public [`ManagerHandle`] / [`ManagerThread`] / [`SpawnError`]
//! contract every driver's `spawn` returns. Both drivers construct their
//! sender via [`manager_channel`] and start the manager thread via
//! [`spawn_manager_thread`]; the split lets the platform-specific listener
//! half wire itself to the same `Arc<Mutex<KeyTracker>>` before the manager
//! thread begins servicing commands.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::tracker::{KeyTracker, RawKeyEvent};

// -----------------------------------------------------------------------
// RawTap — driver-agnostic sink for raw OS key events. Both `rdev_driver`
// and `evdev_driver` invoke `tap()` BEFORE the tracker processes each
// event so the diagnostic `whisper-dictate hotkey capture` CLI can wire
// up a listener that surfaces every keydown/keyup regardless of which
// backend the manager selector picked.
// -----------------------------------------------------------------------

/// Marker type for `spawn_with_raw_tap` callers that don't want a raw tap.
/// Cheaper than an `Option<Arc<dyn Fn>>` — the trait dispatch inlines to a
/// no-op so the shipping supervisor path pays nothing for the diagnostic
/// hook.
pub struct NoopRawTap;

/// Sink for raw OS key events, called BEFORE the tracker processes them.
/// Implemented by [`NoopRawTap`] (drops silently) and any `Fn(&RawKeyEvent) +
/// Send + Sync + 'static` closure. Kept as a trait so both drivers'
/// `spawn_with_raw_tap` can accept either without runtime overhead — the
/// diagnostic `whisper-dictate hotkey capture` CLI wires up a real tap so
/// the operator can see every keydown/keyup, not just the chord-level
/// output the coordinator produces.
///
/// Lives here (not in a driver module) so the trait is identical across
/// backends — otherwise `manager::spawn_with_raw_tap`'s dispatch to
/// `evdev_driver::spawn_with_raw_tap` wouldn't type-check with the R
/// bound coming from the rdev module.
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

/// Public handle to the manager thread. Backend-agnostic: both the rdev and
/// the evdev driver hand one of these back from `spawn`.
#[derive(Clone)]
pub struct ManagerHandle {
    tx: Sender<ManagerCommand>,
    /// Set to `true` at construction; the platform-specific listener
    /// thread flips it to `false` when it exits, which is the only
    /// direct wedge signal we can hand back to callers. Consumed by
    /// [`HotkeyHandle::is_listener_alive`](crate::hotkey::HotkeyHandle::is_listener_alive)
    /// so the boot-self-test can distinguish "install returned Ok and
    /// the listener is still up" from "install returned Ok but the
    /// listener exited before the hold window closed" — the exact
    /// dead-hook regression PR #644 self-test was meant to catch
    /// (Codex P1 #644 discussion r3658983542).
    ///
    /// Backends that cannot observe listener exit (evdev's per-device
    /// readers today, the Windows RegisterHotKey message pump) leave
    /// the atomic at `true`; a `false` reading is always a real
    /// exit signal, never a false alarm.
    listener_alive: Arc<AtomicBool>,
}

impl ManagerHandle {
    /// Handle back to the `Arc<AtomicBool>` the platform listener
    /// thread flips on exit. `pub(crate)` so a driver's spawn function
    /// can wire the same atomic into its listener thread body without
    /// widening the public surface. Consumed by
    /// [`HotkeyHandle::is_listener_alive`](crate::hotkey::HotkeyHandle::is_listener_alive)
    /// via the getter below.
    pub(crate) fn listener_alive_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.listener_alive)
    }

    /// True while the platform listener thread is still running.
    /// Returns `true` on backends that cannot observe listener exit
    /// (they leave the atomic at its default), so callers should treat
    /// a `false` return as an unambiguous "hook is dead" signal but
    /// treat `true` as "no exit observed yet" rather than "guaranteed
    /// healthy". Cheap: one relaxed atomic load.
    pub fn is_listener_alive(&self) -> bool {
        self.listener_alive.load(Ordering::Relaxed)
    }
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
    /// thread (the OS listener stays installed — neither `rdev` nor the
    /// blocking evdev reads give us a clean per-binding teardown — but the
    /// tracker is replaced with an empty one so no events flow through).
    pub fn unregister(&self) -> Result<(), String> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.tx
            .send(ManagerCommand::Unregister { ack: ack_tx })
            .map_err(|e| format!("manager thread disconnected: {e}"))?;
        ack_rx
            .recv()
            .map_err(|e| format!("ack channel closed: {e}"))?
    }

    /// Ask the manager thread to exit. The OS-level listener thread(s) cannot
    /// be interrupted (rdev's `listen()` / evdev's blocking `fetch_events()`
    /// both park forever), so they leak on shutdown — acceptable because the
    /// supervisor only ever installs the hotkey subsystem once and tears down
    /// on process exit.
    pub fn shutdown(&self) {
        let _ = self.tx.send(ManagerCommand::Shutdown);
    }
}

/// Construct a [`ManagerHandle`] plus its inbound receiver. The driver passes
/// the receiver to [`spawn_manager_thread`]; keeping the split here means the
/// listener half can be wired to the same `Arc<Mutex<KeyTracker>>` before the
/// manager thread starts.
///
/// Also installs the shared off-callback diagnostic writer
/// ([`crate::diag::ensure_async_writer`]). This lives HERE — rather
/// than in each backend's `spawn_with_raw_tap` — because every driver
/// funnels through `manager_channel`, so no backend can forget it.
/// Codex P2 #668 discussion 3666165045: the rdev spawn path called
/// `ensure_async_writer` but evdev and win_registerhotkey did not, so
/// on an evdev session with `VOICEPI_LOG=debug` the tracker's
/// `[chord]` trace (routed through `enqueue_async` by the #668
/// 3665741341 fix) silently dropped every message because
/// `ASYNC_QUEUE_TX` was never populated.
///
/// Idempotent and off the hot path: the writer is `OnceLock`-gated and
/// this runs once per install, never from the LL-hook callback.
pub fn manager_channel() -> (ManagerHandle, Receiver<ManagerCommand>) {
    crate::diag::ensure_async_writer();
    let (tx, rx) = mpsc::channel();
    (
        ManagerHandle {
            tx,
            // Codex P2 #668 discussion 3665741337: start `false`. The
            // platform listener thread flips this to `true` ONLY once
            // its OS hook is confirmed installed (rdev: right before
            // `rdev::listen`; win_registerhotkey: right before
            // `run_msg_loop`; evdev: inside `ReaderPopulationFlag::new`
            // once the reader count is known). Previously we defaulted
            // to `true`, but that let `HotkeyHandle::is_listener_alive`
            // report the hook as alive between the moment the listener
            // thread was spawned and the moment it actually reached
            // the OS API — including the entire duration of any
            // pre-listen `diag::log!` stall, which is the exact
            // Windows-stalled-AppData wedge the boot self-test exists
            // to catch. Defaulting to `false` means an "alive"
            // reading now REQUIRES the backend to have observably
            // reached the OS hook; the drop-guard still flips back
            // to `false` on any exit.
            listener_alive: Arc::new(AtomicBool::new(false)),
        },
        rx,
    )
}

/// Owned join handle for the manager thread (NOT the inner OS listener
/// thread, which cannot be joined). Kept by the caller for cleanup.
pub struct ManagerThread {
    join: Option<JoinHandle<()>>,
}

impl ManagerThread {
    pub fn join(mut self) {
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }

    /// Wrap a raw [`JoinHandle`] in a `ManagerThread`. Kept `pub(crate)` so
    /// backends whose manager loop is NOT the generic [`manager_loop`] — the
    /// Windows `RegisterHotKey` driver ([`super::win_registerhotkey`]) owns
    /// its own message pump — can still return the driver-agnostic
    /// [`ManagerThread`] type through `spawn_with_raw_tap`. The field itself
    /// stays private so callers only interact via `join`.
    ///
    /// The `#[cfg_attr(...)]` `allow(dead_code)` silences the unused-fn
    /// warning on non-Windows builds where the only caller
    /// (`win_registerhotkey::spawn_with_raw_tap`) is cfg-gated out.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn from_join(join: JoinHandle<()>) -> Self {
        Self { join: Some(join) }
    }
}

/// Errors a driver's `spawn` surfaces on startup. Translated to
/// `InstallError` by the hotkey module so the supervisor can pick a fallback
/// strategy.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("hotkey listener startup failed: {0}")]
    ListenerStartup(String),
    #[error("hotkey listener thread never reported it was started")]
    ListenerHung,
    /// The off-callback diagnostic writer thread never came up, so the
    /// listener refused to install its OS hook.
    ///
    /// Why this is fatal rather than a warning: every diagnostic that
    /// fires from inside the Windows `WH_KEYBOARD_LL` callback (the rdev
    /// boundary trace, the tracker `[chord]` trace, the parallel
    /// raw-hook trace) goes through `crate::diag::enqueue_async`. With no
    /// writer thread those records are shed silently, and the resulting
    /// `gui-diagnostic.log` is indistinguishable from a healthy quiet
    /// session — so a subsequent PTT wedge report would be undiagnosable.
    /// Failing the spawn lets the supervisor keep the Python listener
    /// wired instead of parking it behind a blind Rust driver.
    #[error("async diagnostic writer failed to start: {0}")]
    WriterStartup(String),
}

/// Spawn the manager thread that owns `tracker` and services register /
/// unregister / shutdown commands off `cmd_rx`. Returns the joinable handle
/// wrapper; the listener half must already share `tracker` via `Arc::clone`.
pub fn spawn_manager_thread(
    cmd_rx: Receiver<ManagerCommand>,
    tracker: Arc<Mutex<KeyTracker>>,
) -> Result<ManagerThread, SpawnError> {
    let join = thread::Builder::new()
        .name("vp-hotkey-manager".to_owned())
        .spawn(move || manager_loop(cmd_rx, tracker))
        .map_err(|e| SpawnError::ListenerStartup(format!("manager thread spawn failed: {e}")))?;
    Ok(ManagerThread { join: Some(join) })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    #[test]
    fn register_and_unregister_roundtrip_through_manager_thread() {
        // Round-trip a Register / Unregister / Register cycle through the
        // manager thread WITHOUT any OS listener attached — proves the
        // shared plumbing is driver-agnostic and does not depend on rdev
        // or evdev being present. Both drivers' `spawn` builds on this
        // exact channel + spawn helper pair.
        let (handle, cmd_rx) = manager_channel();
        let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));
        let thread =
            spawn_manager_thread(cmd_rx, Arc::clone(&tracker)).expect("manager thread spawns");

        handle
            .register(vec!["ctrl_l".to_owned(), "f9".to_owned()])
            .expect("register");
        handle.unregister().expect("unregister");
        handle
            .register(vec!["shift_r".to_owned()])
            .expect("re-register");

        // Give the manager thread a beat, then shutdown.
        handle.shutdown();
        thread.join();
    }

    #[test]
    fn spawn_error_display_variants_carry_context() {
        // The message payload of `ListenerStartup` is what surfaces to the
        // supervisor / CLI, so it must be preserved verbatim through the
        // Display impl. `ListenerHung` has no payload but should still
        // render.
        let msg = SpawnError::ListenerStartup("no X display".to_owned()).to_string();
        assert!(msg.contains("no X display"), "context lost: {msg}");
        let hung = SpawnError::ListenerHung.to_string();
        assert!(!hung.is_empty(), "hung variant must render");
    }

    // -----------------------------------------------------------------------
    // Codex P1 #644 r3658983542 — listener_alive plumbing regression test.
    //
    // The self-test verb's `listener_exited_early` USED to be hardcoded
    // `false`, so a listener that exited during the hold window was
    // silently reported as healthy — the exact class of Windows PTT wedge
    // the verb was written to catch. The fix wires a shared atomic that
    // the platform listener flips to `false` on exit; `ManagerHandle` now
    // exposes it via `is_listener_alive()`, and `HotkeyHandle` re-exports.
    // The test verifies the shared-atomic contract in isolation: a store
    // to `false` via the returned flag Arc becomes visible to
    // `is_listener_alive()`. On un-fixed code the field didn't exist and
    // this test wouldn't compile.
    // -----------------------------------------------------------------------

    #[test]
    fn listener_alive_flag_defaults_false_and_reflects_stores_from_outside_handle() {
        use std::sync::atomic::Ordering;
        let (handle, _rx) = manager_channel();
        // Codex P2 #668 discussion 3665741337: default is now `false`
        // (was `true`). Each backend explicitly flips to `true` only
        // when its OS hook is confirmed installed — otherwise a
        // pre-listen `diag::log!` stall would let the boot self-test
        // pass on a hook that was never installed.
        assert!(
            !handle.is_listener_alive(),
            "freshly-constructed handle must default to `not alive` — \
             the backend explicitly flips to `true` when the OS hook \
             is confirmed installed"
        );
        let flag = handle.listener_alive_flag();
        flag.store(true, Ordering::Relaxed);
        assert!(
            handle.is_listener_alive(),
            "flipping the flag via listener_alive_flag() must be \
             visible through is_listener_alive()"
        );
        flag.store(false, Ordering::Relaxed);
        assert!(
            !handle.is_listener_alive(),
            "flipping the flag back to `false` (the listener exited) \
             must also be visible through is_listener_alive()"
        );
    }

    // -----------------------------------------------------------------------
    // Codex P2 #644 r3659255991 — resume() returning Result plumbing.
    //
    // Before the fix, `HotkeyHandle::resume` swallowed the register
    // failure and returned nothing, so the supervisor's Phase-B "started"
    // line and the ready worker event still fired — Windows tray flipped
    // green even though PTT was silent. The manager-level test below
    // pins the source-of-truth: after `shutdown()`, `register()` on the
    // same manager handle returns Err (the mpsc is disconnected), which
    // is the Err path `HotkeyHandle::resume` now propagates to the
    // in-process supervisor's `HotkeyInstallFailed` fallback.
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Codex P2 #668 discussion 3664983427 — every OS-listener backend
    // MUST wire the shared `listener_alive` flag to its dedicated
    // thread's lifetime, not just the rdev driver. A `self-test
    // hotkey-boot --driver <backend>` run whose listener thread exits
    // or panics during the hold window will otherwise still read
    // `is_listener_alive() == true` and misreport PASS on the exact
    // dead-listener regression the signal exists to catch.
    //
    // Runtime verification for the RegisterHotKey backend lives in
    // `win_registerhotkey_tests::registerhotkey_listener_alive_flag_flips_on_thread_exit`
    // and only runs on Windows. This structural scanner runs on every
    // platform so a Linux/CI push that regresses the wiring is caught
    // before it lands.
    // -----------------------------------------------------------------------

    #[test]
    fn every_backend_source_wires_listener_alive_flag_to_its_thread() {
        use std::fs;
        for (rel_path, backend) in [
            ("src/rust/hotkey/manager/rdev_driver.rs", "rdev"),
            (
                "src/rust/hotkey/manager/win_registerhotkey.rs",
                "win_registerhotkey",
            ),
            // evdev fans out one reader per `/dev/input` device rather
            // than a single listener, but the requirement is the same:
            // the shared `listener_alive` flag must flip when the LAST
            // reader exits so `self-test hotkey-boot --driver evdev`
            // reports the dead-listener wedge (Codex P2 #668
            // discussion 3665369924).
            ("src/rust/hotkey/manager/evdev_driver.rs", "evdev"),
        ] {
            let src = fs::read_to_string(rel_path)
                .or_else(|_| fs::read_to_string(rel_path.trim_start_matches("src/rust/")))
                .unwrap_or_else(|err| {
                    panic!("{backend}: driver source {rel_path} must be readable ({err})")
                });
            // The driver's spawn function must clone the shared alive
            // flag off its `ManagerHandle` — the sole seam that hands
            // the atomic to the listener thread. Without this call the
            // per-backend listener has no way to signal exit.
            assert!(
                src.contains("listener_alive_flag()"),
                "{backend}: spawn must call ManagerHandle::listener_alive_flag() \
                 to obtain the shared atomic; otherwise the listener thread's \
                 exit is invisible to HotkeyHandle::is_listener_alive(). \
                 Codex P2 #668 discussion 3664983427."
            );
            // The listener thread body must actually clear the atomic
            // on exit. A `store(false` occurrence catches both the
            // explicit post-loop store and the drop-guard's `store(false,
            // Ordering::Relaxed)` inside `impl Drop`.
            assert!(
                src.contains("store(false"),
                "{backend}: listener thread must flip listener_alive to false \
                 on exit (typically via a drop-guard so panics count too). \
                 Codex P2 #668 discussion 3664983427."
            );
        }
    }

    // -----------------------------------------------------------------------
    // Codex P2 #668 discussion 3666165045 — every backend must install
    // the shared off-callback diagnostic writer.
    //
    // The #668 3665741341 fix routed the tracker's `[chord]` trace
    // through `crate::diag::enqueue_async`, but only rdev's
    // `spawn_with_raw_tap` called `ensure_async_writer`. On an evdev
    // session (Wayland) or a win_registerhotkey session with
    // `VOICEPI_LOG=debug`, `ASYNC_QUEUE_TX` stayed unset and
    // `enqueue_async` silently dropped EVERY `[chord]` line — exactly
    // the diagnostic the Windows/Wayland PTT investigation depends on.
    //
    // The fix moved the call into `manager_channel()`, the single seam
    // every backend funnels through. These tests pin that: the runtime
    // one proves the call actually happens, the structural one proves
    // no backend re-introduces a private writer-init that could drift.
    // -----------------------------------------------------------------------

    #[test]
    fn manager_channel_installs_the_shared_async_diag_writer() {
        // Constructing a manager channel — which EVERY backend does as
        // its first step — must leave the shared async writer
        // installed, so a later `enqueue_async` from the tracker's
        // `[chord]` branch actually reaches `write_line` instead of
        // being dropped on the floor.
        let (_handle, _rx) = manager_channel();
        assert!(
            crate::diag::async_writer_installed(),
            "manager_channel() must install the shared off-callback diag \
             writer so every backend (rdev / evdev / win_registerhotkey) \
             gets it. Without this, `enqueue_async` silently drops the \
             tracker's `[chord]` trace on non-rdev backends. Codex P2 \
             #668 discussion 3666165045."
        );
    }

    #[test]
    fn every_backend_spawn_funnels_through_manager_channel() {
        // Structural guard for the mechanism that makes the runtime
        // test above cover ALL backends: the writer install lives in
        // `manager_channel()`, so a backend only inherits it by
        // calling `manager_channel()`. Every driver does today; a
        // future backend that built its `ManagerHandle` some other
        // way would silently lose the writer (and the `[chord]`
        // trace with it) — exactly the evdev/win gap Codex P2 #668
        // 3666165045 found, one layer down.
        use std::fs;
        for (rel_path, backend) in [
            ("src/rust/hotkey/manager/rdev_driver.rs", "rdev"),
            ("src/rust/hotkey/manager/evdev_driver.rs", "evdev"),
            (
                "src/rust/hotkey/manager/win_registerhotkey.rs",
                "win_registerhotkey",
            ),
        ] {
            let src = fs::read_to_string(rel_path)
                .or_else(|_| fs::read_to_string(rel_path.trim_start_matches("src/rust/")))
                .unwrap_or_else(|err| {
                    panic!("{backend}: driver source {rel_path} must be readable ({err})")
                });
            // Strip line comments so prose mentioning the helper does
            // not stand in for a real call.
            let code: String = src
                .lines()
                .map(|line| line.find("//").map_or(line, |idx| &line[..idx]))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                code.contains("manager_channel()"),
                "{backend}: spawn must build its ManagerHandle via \
                 `manager_channel()` — that is where the shared \
                 off-callback diag writer is installed, so a backend \
                 that bypasses it silently drops every queued `[chord]` \
                 trace. Codex P2 #668 discussion 3666165045."
            );
        }
    }

    #[test]
    fn register_after_shutdown_reports_manager_disconnect_error() {
        let (handle, cmd_rx) = manager_channel();
        let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));
        let thread = spawn_manager_thread(cmd_rx, tracker).expect("manager spawns");
        handle.shutdown();
        thread.join();
        // Manager thread is gone — the tx.send inside register() must
        // observe the disconnect and return an Err. Without this signal
        // being propagated all the way to `HotkeyHandle::resume` (as its
        // Result<(), String>), the in-process supervisor would still
        // report `state=ready` on a silently-dead resume.
        let err = handle
            .register(vec!["ctrl_l".to_owned()])
            .expect_err("register on a shutdown manager must fail");
        assert!(
            err.contains("manager thread disconnected") || err.contains("ack channel closed"),
            "unexpected error text {err:?} — must reflect the disconnect so the supervisor's \
             HotkeyInstallFailed fallback fires"
        );
    }

    #[test]
    fn shutdown_on_disconnected_handle_is_a_noop() {
        // If the manager thread already exited, `shutdown` must not panic
        // (the send silently fails). This is the drop-order guarantee the
        // `HotkeyHandle` shutdown relies on.
        let (handle, cmd_rx) = manager_channel();
        let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));
        let thread = spawn_manager_thread(cmd_rx, tracker).expect("manager thread spawns");
        handle.shutdown();
        thread.join();
        // Second shutdown after the thread is gone — must not panic.
        handle.shutdown();
        std::thread::sleep(StdDuration::from_millis(1));
    }
}
