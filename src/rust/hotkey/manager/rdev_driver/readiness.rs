//! Listener startup handshake and readiness policy.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Instant;

use super::{SpawnError, READY_PROBE_WINDOW};

/// Signals the listener thread sends to the spawn-side coordinator.
#[derive(Debug)]
pub(crate) enum ListenerSignal {
    /// The thread is up and about to call into rdev.
    Started,
    /// `rdev::listen` returned Err quickly (no display, missing OS
    /// permission, ...). The string is the rdev error formatted for logs.
    Failed(String),
    /// The off-callback diagnostic writer thread never came up, so the
    /// listener aborted BEFORE installing the OS hook. The string is
    /// [`crate::diag::async_writer_result`]'s reason. Mapped to
    /// [`SpawnError::WriterStartup`] by [`await_listener_ready`].
    WriterFailed(String),
}

/// What the listener thread should do after its readiness handshake.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ListenerStart {
    /// The diagnostic pipeline is live; install the OS hook.
    Proceed,
    /// The async writer never spawned. The `WriterFailed` signal is
    /// already on its way to the spawn-side; the listener thread must
    /// return without touching rdev.
    AbortWriterFailed,
}

/// Readiness handshake run on the listener thread: report a dead
/// diagnostic writer, otherwise announce `Started`.
///
/// Order matters. The writer is primed (and its spawn outcome checked)
/// BEFORE `Started` goes out, so a dead writer is a *spawn* failure the
/// caller of `install_hotkey()` sees immediately — rather than something
/// the process discovers hours later as an inexplicably empty
/// `gui-diagnostic.log`. Announcing readiness first and checking after
/// would leave `spawn` returning `Ok` on a permanently blind driver.
///
/// ## Why the abort is conditional (comment 3669770201)
///
/// `callback_diagnostics_enabled` is
/// [`crate::diag::callback_diagnostics_enabled`] in production. Below
/// `info` EVERY enqueue site in the callback below is already switched
/// off by its own `info_enabled` / `debug_enabled` gate, so a writer that
/// failed to spawn is a writer nobody would have used: losing it cannot
/// make the requested log incomplete. Aborting anyway costs the user
/// their working Rust hotkey path — `install_hotkey()` fails,
/// the supervisor falls back to the alternate listener — over a diagnostic
/// that was never going to be written, and at `VOICEPI_LOG=off` the line
/// explaining that is suppressed too, so the downgrade is silent.
///
/// The gate is a parameter rather than a direct `crate::diag` read so
/// `rdev_driver_tests` can drive all four combinations without touching
/// the process-wide `LEVEL` atomic. It mirrors the shape
/// [`crate::hotkey::manager::win_raw_hook::install_gate`] already uses
/// for the same decision on the raw-hook side.
///
/// Pure apart from the channel send, so `rdev_driver_tests` can drive
/// both arms without spawning a listener or an OS hook.
pub(crate) fn listener_readiness_handshake(
    ready_tx: &mpsc::Sender<ListenerSignal>,
    writer_ready: Result<(), String>,
    callback_diagnostics_enabled: bool,
) -> ListenerStart {
    if let Err(msg) = writer_ready {
        if callback_diagnostics_enabled {
            let _ = ready_tx.send(ListenerSignal::WriterFailed(msg));
            return ListenerStart::AbortWriterFailed;
        }
        // Untraced: the hook is worth more than the writer. Say so
        // through the ordinary sink (itself level-gated, so `off` stays
        // silent) rather than failing the install.
        crate::diag::log!(
            "[hotkey/rdev] off-callback diagnostic writer is unavailable ({msg}); \
             installing the OS hook anyway because callback-path tracing is \
             disabled at this log level - raise VOICEPI_LOG to info or higher \
             to make this fatal instead"
        );
    }
    // Announce we're up BEFORE blocking in rdev::listen — without this
    // the spawn-side can't tell "thread never scheduled" apart from
    // "rdev is blocking healthily".
    let _ = ready_tx.send(ListenerSignal::Started);
    ListenerStart::Proceed
}

/// Spawn-side half of the handshake: wait for the listener to report
/// readiness, then give rdev a short window to fail fast.
///
/// Returns `Ok(())` when the listener is healthy, or the
/// [`SpawnError`] to hand back to the caller. Extracted from
/// `spawn_with_raw_tap_inner` so (a) every failure arm funnels through
/// ONE early-return in the caller — which is the only place
/// `heartbeat_stop` has to be signalled, so a new arm cannot forget it
/// and (b) `rdev_driver_tests` can drive the signal-to-`SpawnError`
/// mapping against a plain channel, with no threads and no OS hook.
pub(crate) fn await_listener_ready(
    ready_rx: &mpsc::Receiver<ListenerSignal>,
) -> Result<(), SpawnError> {
    match ready_rx.recv_timeout(READY_PROBE_WINDOW) {
        Ok(ListenerSignal::Started) => {}
        Ok(ListenerSignal::Failed(msg)) => return Err(SpawnError::ListenerStartup(msg)),
        Ok(ListenerSignal::WriterFailed(msg)) => return Err(SpawnError::WriterStartup(msg)),
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
            Ok(ListenerSignal::WriterFailed(msg)) => return Err(SpawnError::WriterStartup(msg)),
            Ok(ListenerSignal::Started) => {} // duplicate, ignore
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break, // listener exited without err — healthy
        }
    }
    Ok(())
}
