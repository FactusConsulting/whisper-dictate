//! One native listener lifetime, including readiness and liveness ordering.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use super::super::tracker::TrackerOutput;
use super::callback::CallbackContext;
use super::readiness::{listener_readiness_handshake, ListenerSignal, ListenerStart};
use super::RawTap;

pub(super) fn run_native_listener<F, R>(
    callback: CallbackContext<F, R>,
    listener_alive_for_thread: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<ListenerSignal>,
) where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    // Drop-guard: flips the shared liveness flag to false when
    // this thread ends for ANY reason — normal return from
    // `rdev::listen`, an Err quick-failure, or a panic. Without
    // this the boot-self-test regression signal would miss the
    // panic case (the two explicit stores below only cover the
    // two `rdev::listen` return arms). The two explicit stores
    // are kept so a future refactor that drops this guard still
    // preserves the primary happy-path signal; storing `false`
    // twice is a no-op on a one-shot latch.
    // discussion r3658983542.
    struct AliveGuard(Arc<AtomicBool>);
    impl Drop for AliveGuard {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Relaxed);
        }
    }
    let _alive_guard = AliveGuard(Arc::clone(&listener_alive_for_thread));
    // Prime the off-callback diagnostic writer BEFORE announcing
    // readiness. `manager_channel()` already called
    // `ensure_async_writer`, so this is normally a no-op read of
    // the recorded outcome — but it is the point where a
    // FAILED writer spawn becomes visible, and it has to happen
    // before `Started` goes out or `spawn` would return Ok on a
    // driver whose every callback diagnostic is silently shed.
    let writer_ready = crate::diag::async_writer_result();
    if let ListenerStart::AbortWriterFailed = listener_readiness_handshake(
        &ready_tx,
        writer_ready,
        crate::diag::callback_diagnostics_enabled(),
    ) {
        // No OS hook is installed on this path, and the
        // AliveGuard above already latches the liveness flag
        // back to false on return.
        return;
    }
    // Diagnostic marker so the tee file records the listener
    // thread actually reached rdev::listen. Combined with the
    // heartbeat, "startup log line present + heartbeat present
    // but events_since_last_heartbeat=0" is the Windows PTT
    // wedge signature (hook installed, pump idle).
    crate::diag::log!(
        "[hotkey/rdev] listener thread started; installing global hook \
         (WH_KEYBOARD_LL on Windows / XRecord on X11 / CGEventTap on macOS)"
    );
    // discussion 3665741337: flip the alive
    // flag to `true` HERE — after any potentially-stalling
    // pre-listen `diag::log!` has returned, and just before
    // we enter `rdev::listen` where the hook is actually
    // installed. The manager-channel default is `false`, so
    // if the log call above stalls (blocked AppData I/O on
    // Windows), the flag stays `false` and the boot self-test
    // correctly reports `listener_exited_early:true` for a
    // hook that was never installed. On the healthy path the
    // spawn-side waits READY_PROBE_WINDOW after `Started`
    // before returning, so this store lands before any
    // external observer polls `is_listener_alive()`.
    listener_alive_for_thread.store(true, Ordering::Relaxed);
    let cb = move |event: rdev::Event| callback.handle_event(event);
    let listen_result = rdev::listen(cb);
    // discussion 3664983439: flip the liveness
    // atomic BEFORE any synchronous `diag::log!` call. Ordering
    // matters — if the diagnostic sink is stalled (blocked
    // AppData I/O on Windows), a boot-self-test polling
    // `is_listener_alive()` during the hold window would still
    // read `true` and misreport PASS on the exact dead-hook
    // condition this signal exists to detect. The hook is
    // already dead by the moment `rdev::listen` returns, so
    // observing that fact must not wait on I/O.
    listener_alive_for_thread.store(false, Ordering::Relaxed);
    if let Err(err) = listen_result {
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
}
