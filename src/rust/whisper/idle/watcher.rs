//! Background watcher loop, unload-decision helper, and RAII load guard.
//!
//! Split out from the public wrapper so the unload-decision can be unit
//! tested deterministically (it accepts a hook between the "is the window
//! elapsed" pre-check and the post-lock re-check, which lets a test prove
//! the re-check actually protects late activity arriving from another
//! thread).

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::Shared;

/// How often the watcher thread wakes up to check the idle clock.
///
/// We poll rather than schedule a one-shot timer because `note_activity` can
/// reset the deadline at any moment; recomputing a sleep duration on every
/// activity hit (and dealing with the cross-thread signalling that requires)
/// is more code and more failure modes than a 250 ms heartbeat. The cost is
/// at most ~250 ms of extra resident time after the idle window expires,
/// which is irrelevant for a model whose load latency is measured in seconds.
const WATCHER_TICK: Duration = Duration::from_millis(250);

/// RAII guard that toggles `Shared::loading` true→false across a load.
///
/// Construction sets the flag; `Drop` clears it. This makes a panic in the
/// loader closure safe: the watcher sees a consistent state immediately
/// after unwind, and the next `with_model` call can retry without being
/// blocked by a stale "loading in progress" sentinel.
pub(super) struct LoadGuard<'a, M> {
    shared: &'a Shared<M>,
}

impl<'a, M> LoadGuard<'a, M> {
    pub(super) fn new(shared: &'a Shared<M>) -> Self {
        shared.loading.store(true, Ordering::Release);
        Self { shared }
    }
}

impl<M> Drop for LoadGuard<'_, M> {
    fn drop(&mut self) {
        self.shared.loading.store(false, Ordering::Release);
    }
}

/// Background loop: every [`WATCHER_TICK`], check whether the idle window
/// has elapsed and unload the model if so. Exits when `shutdown` flips true.
pub(super) fn watcher_loop<M>(
    shared: Arc<Shared<M>>,
    shutdown: Arc<AtomicBool>,
    timeout: Duration,
) {
    let timeout_ms = timeout.as_millis() as i64;
    loop {
        // Sleep first so a freshly-built wrapper doesn't unload before the
        // very first `with_model` call even gets a chance to run.
        thread::sleep(WATCHER_TICK);
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        if shared.loading.load(Ordering::Acquire) {
            // A load is in progress — don't fight it for the mutex.
            continue;
        }
        try_idle_unload(&shared, timeout_ms);
    }
}

/// Returns `true` iff the idle window has elapsed since the last recorded
/// activity. Pure read; cheap enough to call twice per watcher tick.
fn idle_window_elapsed<M>(shared: &Shared<M>, timeout_ms: i64) -> bool {
    let last = shared.last_activity_ms.load(Ordering::Relaxed);
    now_ms().saturating_sub(last) >= timeout_ms
}

/// Try to drop the model if the idle window has elapsed; returns `true`
/// iff a model was actually unloaded.
///
/// Production callers should use this entry point — it forwards to
/// [`try_idle_unload_with_hook`] with a no-op hook. Tests that need to
/// observe the re-check window between the pre-lock decision and the
/// post-lock re-check call the hook variant directly.
pub(super) fn try_idle_unload<M>(shared: &Shared<M>, timeout_ms: i64) -> bool {
    try_idle_unload_with_hook(shared, timeout_ms, || {})
}

/// Core unload decision: pre-check the idle window without the lock, grab
/// the model mutex (or bail on contention), then **re-check** the idle
/// window inside the lock before actually taking the model.
///
/// The re-check is the load-bearing safety: between the unlocked
/// observation and the lock acquisition, another thread (typically the
/// hotkey path calling `note_activity()`) can refresh the activity clock
/// to protect a model the user is about to need. Without the re-check the
/// watcher would drop the model based on a stale snapshot and force an
/// avoidable lazy reload on the very next call — defeating the
/// hotkey/startup use case the wrapper exists to support.
fn try_idle_unload_with_hook<M>(
    shared: &Shared<M>,
    timeout_ms: i64,
    on_locked: impl FnOnce(),
) -> bool {
    if !idle_window_elapsed(shared, timeout_ms) {
        return false;
    }
    // `try_lock` so we don't block on an in-flight transcribe; we'll catch
    // the next tick if it's busy.
    let mut guard = match shared.model.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return false,
    };
    on_locked();
    // Re-check inside the lock. See function-level comment for the rationale.
    if !idle_window_elapsed(shared, timeout_ms) {
        return false;
    }
    let taken = guard.take();
    let dropped = taken.is_some();
    drop(guard);
    drop(taken);
    dropped
}

/// Unix epoch milliseconds, monotonic-ish for our purposes. We use system
/// time (not [`Instant`]) so the value is comparable across threads and
/// survives clock-source quirks on cheap timers. The watcher tolerates the
/// rare backward jump (clock adjustment, NTP step) because it only triggers
/// an early/late unload, never a panic.
pub(super) fn now_ms() -> i64 {
    // `Instant` would be the obvious choice for a "no time travel" guarantee,
    // but the existing whisper module doesn't need Instant elsewhere and the
    // worst case of system-clock skew here is at most a single spurious
    // unload (or, equivalently, one extra idle window). That is far cheaper
    // than the boilerplate of plumbing a monotonic clock through atomic
    // state.
    let _ = Instant::now(); // touch the import so future refactors keep it
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test stand-in. Carries no state; we only care about presence/absence
    /// in the mutex slot.
    struct FakeModel;

    fn shared_with_model() -> Arc<Shared<FakeModel>> {
        let shared: Arc<Shared<FakeModel>> = Arc::new(Shared::new());
        *shared.model.lock().unwrap() = Some(FakeModel);
        shared
    }

    #[test]
    fn load_guard_sets_and_clears_loading_flag() {
        let shared: Shared<FakeModel> = Shared::new();
        assert!(!shared.loading.load(Ordering::Acquire));
        {
            let _g = LoadGuard::new(&shared);
            assert!(shared.loading.load(Ordering::Acquire));
        }
        assert!(!shared.loading.load(Ordering::Acquire));
    }

    #[test]
    fn idle_window_elapsed_false_when_activity_is_fresh() {
        let shared = shared_with_model();
        shared.last_activity_ms.store(now_ms(), Ordering::Relaxed);
        assert!(!idle_window_elapsed(&shared, 60_000));
    }

    #[test]
    fn idle_window_elapsed_true_when_activity_is_stale() {
        let shared = shared_with_model();
        // Epoch zero — definitely more than 100 ms ago.
        shared.last_activity_ms.store(0, Ordering::Relaxed);
        assert!(idle_window_elapsed(&shared, 100));
    }

    #[test]
    fn try_idle_unload_skips_when_activity_is_fresh() {
        let shared = shared_with_model();
        shared.last_activity_ms.store(now_ms(), Ordering::Relaxed);
        let dropped = try_idle_unload(&shared, 60_000);
        assert!(!dropped, "should not unload — activity is fresh");
        assert!(
            shared.model.lock().unwrap().is_some(),
            "model must remain resident"
        );
    }

    #[test]
    fn try_idle_unload_drops_when_window_elapsed() {
        let shared = shared_with_model();
        shared.last_activity_ms.store(0, Ordering::Relaxed);
        let dropped = try_idle_unload(&shared, 100);
        assert!(dropped, "should unload — window long elapsed");
        assert!(
            shared.model.lock().unwrap().is_none(),
            "model slot must be empty after unload"
        );
    }

    /// The load-bearing race test: between the pre-lock "is the idle window
    /// elapsed?" check and the post-lock re-check, simulate the hotkey path
    /// calling `note_activity()`. Without the re-check the watcher would
    /// unload despite the user's just-arrived intent to dictate.
    #[test]
    fn try_idle_unload_rechecks_after_acquiring_lock() {
        let shared = shared_with_model();
        // Pre-check sees stale activity → would otherwise proceed to unload.
        shared.last_activity_ms.store(0, Ordering::Relaxed);

        let shared_for_hook = Arc::clone(&shared);
        let dropped = try_idle_unload_with_hook(&shared, 100, move || {
            // Simulate `note_activity()` racing in between the pre-check and
            // the post-lock re-check.
            shared_for_hook
                .last_activity_ms
                .store(now_ms(), Ordering::Relaxed);
        });

        assert!(
            !dropped,
            "re-check must protect a model whose activity was refreshed mid-decision"
        );
        assert!(
            shared.model.lock().unwrap().is_some(),
            "model must remain resident after re-check"
        );
    }

    /// Counterpart: if the hook does NOT bump activity, the re-check still
    /// agrees with the pre-check and the model is unloaded. Confirms the
    /// re-check is not over-conservative.
    #[test]
    fn try_idle_unload_drops_when_recheck_still_expired() {
        let shared = shared_with_model();
        shared.last_activity_ms.store(0, Ordering::Relaxed);

        let dropped = try_idle_unload_with_hook(&shared, 100, || {
            // Intentionally leave activity stale.
        });

        assert!(
            dropped,
            "re-check should still allow unload when activity stays stale"
        );
        assert!(shared.model.lock().unwrap().is_none());
    }
}
