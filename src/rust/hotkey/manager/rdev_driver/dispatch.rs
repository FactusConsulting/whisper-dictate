//! Process-wide native listener lifecycle and per-install event dispatch.
//!
//! `rdev::listen` has no stop API. Every call installs another OS hook
//! (WH_KEYBOARD_LL + WH_MOUSE_LL on Windows) and overwrites rdev's single
//! global callback, so calling it once per hotkey install would stack hooks
//! that all feed the newest callback. The driver instead keeps at most one
//! live listener thread per process ([`NativeListener`]) and routes its
//! events through [`SinkSlot`]: each install swaps its own sink in, and its
//! teardown clears it, which leaves the OS hooks in place as no-ops.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::JoinHandle;

use super::SpawnError;

/// Destination for events delivered by the process-wide listener.
pub(super) type EventSink<E> = Arc<dyn Fn(E) + Send + Sync>;

/// Sink that receives every event from the process-wide rdev listener.
pub(super) static EVENT_SINK: SinkSlot<rdev::Event> = SinkSlot::new();

/// The process-wide rdev listener thread.
pub(super) static NATIVE_LISTENER: LazyLock<NativeListener> = LazyLock::new(NativeListener::new);

/// Identifies one [`SinkSlot::install`] so a teardown can only clear its
/// own sink, never one a later install swapped in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SinkToken(u64);

/// Swappable slot holding the active install's event sink.
pub(super) struct SinkSlot<E> {
    current: Mutex<Option<(SinkToken, EventSink<E>)>>,
    next_token: AtomicU64,
}

impl<E> SinkSlot<E> {
    pub(super) const fn new() -> Self {
        Self {
            current: Mutex::new(None),
            next_token: AtomicU64::new(0),
        }
    }

    /// Make `sink` the only receiver of dispatched events, replacing any
    /// previous sink.
    pub(super) fn install(&self, sink: EventSink<E>) -> SinkToken {
        let token = SinkToken(self.next_token.fetch_add(1, Ordering::Relaxed));
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = Some((token, sink));
        token
    }

    /// Drop the sink installed under `token`. A no-op when a later install
    /// has already replaced it.
    pub(super) fn clear(&self, token: SinkToken) {
        let mut current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if current.as_ref().is_some_and(|(active, _)| *active == token) {
            *current = None;
        }
    }

    /// Deliver `event` to the current sink, or drop it when none is
    /// installed. The lock covers only the `Arc` clone, so the sink runs
    /// without holding it and a concurrent install or clear never waits on
    /// user code.
    pub(super) fn dispatch(&self, event: E) {
        let sink = self
            .current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|(_, sink)| Arc::clone(sink));
        if let Some(sink) = sink {
            sink(event);
        }
    }
}

/// Start-at-most-once guard for the native listener thread.
///
/// A new thread is started only when no listener thread exists yet or the
/// previous one has exited; an exited thread has already released its OS
/// hooks, so starting another cannot stack them.
pub(super) struct NativeListener {
    /// Liveness flag shared by the listener thread and every install's
    /// `ManagerHandle`.
    alive: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl NativeListener {
    pub(super) fn new() -> Self {
        Self {
            alive: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
        }
    }

    pub(super) fn alive_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.alive)
    }

    /// Ensure a listener thread is running. `start` receives the liveness
    /// flag, spawns the thread and runs the readiness handshake; it is only
    /// called when no listener thread is running. The lock is held across
    /// `start`, so concurrent installs wait for one handshake instead of
    /// each starting a listener.
    ///
    /// A running listener whose liveness flag is still `false` has not
    /// reached its OS hook (its startup hung), so reusing it reports
    /// [`SpawnError::ListenerHung`].
    pub(super) fn ensure_started<S>(&self, start: S) -> Result<(), SpawnError>
    where
        S: FnOnce(Arc<AtomicBool>) -> (Option<JoinHandle<()>>, Result<(), SpawnError>),
    {
        let mut thread = self.thread.lock().unwrap_or_else(|p| p.into_inner());
        if thread.as_ref().is_some_and(|t| !t.is_finished()) {
            return if self.alive.load(Ordering::Relaxed) {
                Ok(())
            } else {
                Err(SpawnError::ListenerHung)
            };
        }
        let (started, ready) = start(Arc::clone(&self.alive));
        *thread = started;
        ready
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
