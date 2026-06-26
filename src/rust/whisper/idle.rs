//! Idle-aware lazy loader for the local Whisper model (#325, Wave 7-A).
//!
//! A loaded `LocalWhisper` holds the full GGML model in RAM (1-2 GB for
//! `small`/`medium`). For desktop dictation, that resident footprint is wasted
//! between hotkey presses: a user who dictates twice a day still pays the full
//! cost the other 23 hours. This module wraps a model behind a configurable
//! idle timer — after `N` seconds without activity, a background watcher
//! thread drops the model and returns its RAM. The next transcribe call lazily
//! reloads from disk.
//!
//! ## Design knobs
//!
//! - **Idle timeout.** Set via [`parse_idle_timeout_from_env`] from the
//!   `VOICEPI_WHISPER_IDLE_UNLOAD_S` environment variable. `0` (or unset)
//!   means *never unload* — that is the historical behaviour, so a default
//!   build is byte-identical to today.
//! - **Activity extends the timer.** Both [`IdleUnloadingModel::with_model`]
//!   and the explicit [`IdleUnloadingModel::note_activity`] hook (intended
//!   for the hotkey press, before the recording even starts) stamp the
//!   last-activity clock. The watcher only unloads when *now − last_activity
//!   ≥ idle_timeout*.
//! - **Lazy reload.** A subsequent `with_model` call after an unload
//!   transparently re-runs the loader. The caller never sees an explicit
//!   "model unloaded" error — they just pay the reload latency.
//! - **Poisoned-mutex recovery.** A panic inside `with_model`'s callback
//!   would poison the model mutex. We unwrap via `into_inner` so the next
//!   call still finds the (untouched) model rather than refusing forever.
//! - **RAII load discipline.** [`LoadGuard`] enforces that the "currently
//!   loading" book-keeping is reset on every exit path — Ok, Err, or panic.
//!   That keeps `is_loaded()` truthful even if the loader itself panics.
//!
//! The wrapper is **generic over the model type** so the unit tests can
//! exercise the lifecycle (load → idle → unload → reload, activity-extension,
//! poison recovery) without needing a real 75 MB GGML file at test time —
//! see the `FakeModel` tests at the bottom.
//!
//! This module deliberately does not wire itself into the runtime: the
//! `whisper-rs-local` subprocess dispatcher (`whisper::dispatch`) spawns a
//! fresh process per transcribe and therefore cannot benefit from in-process
//! caching. The wrapper is the library primitive a future in-process worker
//! will reach for; landing it now (per the Wave 7-A roadmap entry) lets the
//! later wiring PR stay tiny and focused.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};

use crate::whisper::LocalWhisper;

/// Environment variable that controls the idle-unload timer.
///
/// Value semantics (parsed by [`parse_idle_timeout_from_env`]):
/// - **Unset or empty** → `None` (never unload — historical behaviour).
/// - **`"0"`** → `None` (explicit opt-out, easier to wire from a UI dropdown
///   whose "Never" option emits `0` rather than removing the variable).
/// - **Positive integer** → `Some(Duration::from_secs(n))`.
///
/// Anything else (negative, non-numeric, decimal) is a hard error rather
/// than a silent fallback — the user has expressed an intent, and silently
/// reinterpreting it as "never" would mask configuration bugs in the wrapper
/// that sets the variable.
pub const IDLE_UNLOAD_ENV: &str = "VOICEPI_WHISPER_IDLE_UNLOAD_S";

/// How often the watcher thread wakes up to check the idle clock.
///
/// We poll rather than schedule a one-shot timer because `note_activity` can
/// reset the deadline at any moment; recomputing a sleep duration on every
/// activity hit (and dealing with the cross-thread signalling that requires)
/// is more code and more failure modes than a 250 ms heartbeat. The cost is
/// at most ~250 ms of extra resident time after the idle window expires,
/// which is irrelevant for a model whose load latency is measured in seconds.
const WATCHER_TICK: Duration = Duration::from_millis(250);

/// Shared mutable state between the public handle and the watcher thread.
///
/// Lives behind an `Arc` so the watcher (which outlives `IdleUnloadingModel`
/// only briefly, during shutdown join) keeps a stable reference.
struct Shared<M> {
    /// The model itself. `None` means "unloaded — reload on demand". A
    /// poisoned lock is recovered via `into_inner` because the model itself
    /// is immutable from our perspective and a panic inside a user callback
    /// hasn't corrupted it.
    model: Mutex<Option<M>>,
    /// Last-activity timestamp as Unix epoch milliseconds. We use `i64`
    /// (signed) so the rare pre-epoch system clock state doesn't underflow
    /// silently; the watcher tolerates any value because it only compares
    /// against `now()`.
    last_activity_ms: AtomicI64,
    /// `true` while a load is in progress. Prevents the watcher from racing
    /// the loader to drop the slot before it gets populated. The
    /// [`LoadGuard`] RAII handle is the only thing that toggles this.
    loading: AtomicBool,
}

impl<M> Shared<M> {
    fn new() -> Self {
        Self {
            model: Mutex::new(None),
            last_activity_ms: AtomicI64::new(now_ms()),
            loading: AtomicBool::new(false),
        }
    }
}

/// Background-watcher controlled wrapper that lazily loads a model on demand
/// and unloads it after a configurable idle window.
///
/// Generic over the model type so tests can substitute a cheap fake; the
/// production [`for_local_whisper`](Self::for_local_whisper) constructor pins
/// `M = LocalWhisper`.
pub struct IdleUnloadingModel<M: Send + 'static> {
    /// Loader closure stored in an `Arc` so the (rare) reload path doesn't
    /// have to clone the underlying captures.
    loader: Arc<dyn Fn() -> Result<M> + Send + Sync>,
    shared: Arc<Shared<M>>,
    idle_timeout: Option<Duration>,
    /// Set to `true` from `Drop` to ask the watcher thread to exit.
    shutdown: Arc<AtomicBool>,
    /// `Option` so `Drop` can `take()` the handle and join the watcher
    /// without consuming `self`.
    watcher: Option<JoinHandle<()>>,
}

impl<M: Send + 'static> IdleUnloadingModel<M> {
    /// Build an idle-unloading wrapper around a loader closure.
    ///
    /// `idle_timeout = None` disables the watcher entirely (never unload).
    /// Any `Some(Duration)` spawns the watcher thread; pass
    /// [`parse_idle_timeout_from_env`]`()?` for the production wiring.
    pub fn new(
        loader: impl Fn() -> Result<M> + Send + Sync + 'static,
        idle_timeout: Option<Duration>,
    ) -> Self {
        let loader: Arc<dyn Fn() -> Result<M> + Send + Sync> = Arc::new(loader);
        let shared = Arc::new(Shared::new());
        let shutdown = Arc::new(AtomicBool::new(false));
        let watcher = idle_timeout.map(|timeout| {
            let shared = Arc::clone(&shared);
            let shutdown = Arc::clone(&shutdown);
            thread::Builder::new()
                .name("whisper-idle-unloader".into())
                .spawn(move || watcher_loop(shared, shutdown, timeout))
                .expect("spawn whisper-idle-unloader thread")
        });
        Self {
            loader,
            shared,
            idle_timeout,
            shutdown,
            watcher,
        }
    }

    /// Extend the idle timer without touching the model.
    ///
    /// Intended for the hotkey-press path: the user has signalled intent to
    /// dictate, but the recorder is still spooling up audio. Bumping the
    /// activity clock here keeps the watcher from unloading a model the user
    /// is about to need.
    pub fn note_activity(&self) {
        self.shared
            .last_activity_ms
            .store(now_ms(), Ordering::Relaxed);
    }

    /// `true` iff the model is currently resident in RAM.
    ///
    /// Note that this is a snapshot: the watcher thread may unload the model
    /// the moment this returns. Use it for UI/telemetry, not for control
    /// flow. (Control flow happens implicitly through [`with_model`](Self::with_model),
    /// which lazy-reloads as needed.)
    pub fn is_loaded(&self) -> bool {
        // Poisoned-mutex recovery: a panic in a callback poisons the lock but
        // the data itself is fine — surfacing the inner state is the right
        // call for an observability accessor.
        match self.shared.model.lock() {
            Ok(g) => g.is_some(),
            Err(poisoned) => poisoned.into_inner().is_some(),
        }
    }

    /// Eagerly drop the loaded model, returning the RAM immediately.
    ///
    /// Exposed for tests and for a "Free RAM now" UI button. A subsequent
    /// `with_model` call will lazy-reload.
    pub fn unload(&self) {
        let mut guard = match self.shared.model.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Take and drop outside the brace block so the Mutex is unlocked
        // before any Drop side-effects (e.g. closing a GPU context) run.
        let taken = guard.take();
        drop(guard);
        drop(taken);
    }

    /// Run a closure against the loaded model, loading it first if needed.
    ///
    /// Stamps the activity clock both *before* loading (so a slow reload
    /// can't itself trigger an idle unload mid-call) and *after* the
    /// callback returns. If `loader` returns an error the slot stays empty
    /// and the next call will retry the load.
    pub fn with_model<R>(&self, f: impl FnOnce(&M) -> Result<R>) -> Result<R> {
        // Bump first: even if loading takes seconds, we don't want the
        // watcher to decide it can unload between the lock acquisition and
        // the lazy load below.
        self.note_activity();

        let mut guard = match self.shared.model.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.is_none() {
            // RAII guard: even if `loader` panics, the `loading` flag is
            // cleared on unwind. Without this the watcher would refuse to
            // ever unload again (loading "stuck on") and `is_loaded` could
            // diverge from reality.
            let _load_guard = LoadGuard::new(&self.shared);
            let model = (self.loader)().context("failed to lazy-load whisper model")?;
            *guard = Some(model);
        }
        let model = guard
            .as_ref()
            .expect("model slot populated above (or returned Err)");
        let out = f(model);
        // Re-stamp on the way out so the idle clock measures "time since last
        // *completion*", not "time since last *start*". A long inference
        // shouldn't immediately make itself eligible for unload.
        self.note_activity();
        out
    }

    /// The configured idle timeout, if any.
    ///
    /// Exposed so the UI can display the active setting without having to
    /// re-parse the env var.
    pub fn idle_timeout(&self) -> Option<Duration> {
        self.idle_timeout
    }
}

impl IdleUnloadingModel<LocalWhisper> {
    /// Convenience constructor for the production wiring: pin `M` to
    /// [`LocalWhisper`] and build the loader from a model path.
    ///
    /// The model path is cloned into the closure so the wrapper owns its
    /// load configuration outright — callers don't have to worry about
    /// keeping the path buffer alive across the (unbounded) idle/reload
    /// cycle.
    pub fn for_local_whisper(
        model_path: std::path::PathBuf,
        idle_timeout: Option<Duration>,
    ) -> Self {
        Self::new(move || LocalWhisper::new(&model_path), idle_timeout)
    }
}

impl<M: Send + 'static> Drop for IdleUnloadingModel<M> {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(h) = self.watcher.take() {
            // Ignore join errors: the watcher panic is a bug worth knowing
            // about at run time but should not itself panic our Drop. A
            // panic during Drop while another panic is unwinding aborts the
            // process, which is strictly worse than swallowing.
            let _ = h.join();
        }
    }
}

/// RAII guard that toggles `Shared::loading` true→false across a load.
///
/// Construction sets the flag; `Drop` clears it. This makes a panic in the
/// loader closure safe: the watcher sees a consistent state immediately
/// after unwind, and the next `with_model` call can retry without being
/// blocked by a stale "loading in progress" sentinel.
struct LoadGuard<'a, M> {
    shared: &'a Shared<M>,
}

impl<'a, M> LoadGuard<'a, M> {
    fn new(shared: &'a Shared<M>) -> Self {
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
fn watcher_loop<M>(shared: Arc<Shared<M>>, shutdown: Arc<AtomicBool>, timeout: Duration) {
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
        let last = shared.last_activity_ms.load(Ordering::Relaxed);
        let elapsed = now_ms().saturating_sub(last);
        if elapsed < timeout_ms {
            continue;
        }
        // Idle expired — drop the model. `try_lock` so we don't block on an
        // in-flight transcribe; we'll catch the next tick if it's busy.
        let mut guard = match shared.model.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => continue,
        };
        let taken = guard.take();
        drop(guard);
        drop(taken);
    }
}

/// Read [`IDLE_UNLOAD_ENV`] and parse it into an optional idle window.
///
/// See the constant's docs for the value grammar.
pub fn parse_idle_timeout_from_env() -> Result<Option<Duration>> {
    match std::env::var(IDLE_UNLOAD_ENV) {
        Err(_) => Ok(None),
        Ok(raw) => parse_idle_timeout_str(&raw),
    }
}

/// Pure helper for the env-var parser, split out for unit testing without
/// having to mutate the process environment.
fn parse_idle_timeout_str(raw: &str) -> Result<Option<Duration>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let secs: u64 = trimmed.parse().map_err(|_| {
        anyhow!(
            "{IDLE_UNLOAD_ENV}={raw:?} is not a non-negative integer; \
             use 0 for 'never unload' or a positive seconds count"
        )
    })?;
    if secs == 0 {
        Ok(None)
    } else {
        Ok(Some(Duration::from_secs(secs)))
    }
}

/// Unix epoch milliseconds, monotonic-ish for our purposes. We use system
/// time (not [`Instant`]) so the value is comparable across threads and
/// survives clock-source quirks on cheap timers. The watcher tolerates the
/// rare backward jump (clock adjustment, NTP step) because it only triggers
/// an early/late unload, never a panic.
fn now_ms() -> i64 {
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
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    use crate::test_env_lock::ENV_LOCK;

    /// Tiny stand-in for `LocalWhisper` so tests don't need a 75 MB GGML.
    /// Carries a unique generation number so the assertion `model reloaded`
    /// can prove a fresh instance came back rather than a cached one.
    struct FakeModel {
        generation: usize,
    }

    /// Factory returning a fresh `FakeModel` per call with a monotonic
    /// generation counter. Wrapped in `Arc` so the closure is `Sync`.
    fn fake_loader() -> impl Fn() -> Result<FakeModel> + Send + Sync {
        let counter = Arc::new(AtomicUsize::new(0));
        move || {
            let generation = counter.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(FakeModel { generation })
        }
    }

    /// Spin until `cond` returns true or `budget` elapses. Used in lieu of
    /// rigid `thread::sleep` so the unload-after-idle tests aren't flaky
    /// under CI scheduler jitter.
    fn poll_until<F: FnMut() -> bool>(mut cond: F, budget: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < budget {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        cond()
    }

    #[test]
    fn parse_env_unset_means_never() {
        assert_eq!(parse_idle_timeout_str("").unwrap(), None);
        assert_eq!(parse_idle_timeout_str("   ").unwrap(), None);
    }

    #[test]
    fn parse_env_zero_means_never() {
        assert_eq!(parse_idle_timeout_str("0").unwrap(), None);
        assert_eq!(parse_idle_timeout_str("  0 ").unwrap(), None);
    }

    #[test]
    fn parse_env_positive_returns_duration() {
        assert_eq!(
            parse_idle_timeout_str("30").unwrap(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_idle_timeout_str("3600").unwrap(),
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn parse_env_rejects_negative() {
        let err = parse_idle_timeout_str("-5").unwrap_err();
        assert!(
            err.to_string().contains("non-negative"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_env_rejects_non_numeric() {
        let err = parse_idle_timeout_str("forever").unwrap_err();
        assert!(
            err.to_string().contains("non-negative"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_env_rejects_decimal() {
        // We accept seconds-only (matching the UI dropdown). Decimal is
        // rejected with the same error rather than silently truncated.
        let err = parse_idle_timeout_str("1.5").unwrap_err();
        assert!(
            err.to_string().contains("non-negative"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_env_from_process_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var(IDLE_UNLOAD_ENV).ok();
        std::env::remove_var(IDLE_UNLOAD_ENV);

        assert_eq!(parse_idle_timeout_from_env().unwrap(), None);

        if let Some(v) = saved {
            std::env::set_var(IDLE_UNLOAD_ENV, v);
        }
    }

    #[test]
    fn parse_env_from_process_set() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var(IDLE_UNLOAD_ENV).ok();
        std::env::set_var(IDLE_UNLOAD_ENV, "42");

        assert_eq!(
            parse_idle_timeout_from_env().unwrap(),
            Some(Duration::from_secs(42))
        );

        match saved {
            Some(v) => std::env::set_var(IDLE_UNLOAD_ENV, v),
            None => std::env::remove_var(IDLE_UNLOAD_ENV),
        }
    }

    #[test]
    fn never_unload_when_timeout_is_none() {
        // No watcher thread is spawned; the model must stay resident
        // indefinitely once loaded.
        let model = IdleUnloadingModel::<FakeModel>::new(fake_loader(), None);
        let gen = model.with_model(|m| Ok(m.generation)).unwrap();
        assert_eq!(gen, 1);
        // Wait long enough that any plausible watcher would have fired.
        thread::sleep(Duration::from_millis(400));
        assert!(model.is_loaded(), "model unloaded despite timeout=None");
        // Second call must reuse the same instance — no reload happened.
        let gen2 = model.with_model(|m| Ok(m.generation)).unwrap();
        assert_eq!(gen2, 1, "model reloaded despite timeout=None");
    }

    #[test]
    fn unloads_after_idle_window() {
        let model =
            IdleUnloadingModel::<FakeModel>::new(fake_loader(), Some(Duration::from_millis(150)));
        model.with_model(|_| Ok(())).unwrap();
        assert!(model.is_loaded(), "model should be loaded after with_model");

        // After idle window + a couple of watcher ticks the model must be gone.
        let unloaded = poll_until(|| !model.is_loaded(), Duration::from_secs(2));
        assert!(unloaded, "watcher failed to unload model after idle window");
    }

    #[test]
    fn lazy_reloads_after_unload() {
        let model =
            IdleUnloadingModel::<FakeModel>::new(fake_loader(), Some(Duration::from_millis(150)));
        let g1 = model.with_model(|m| Ok(m.generation)).unwrap();
        assert_eq!(g1, 1);
        let unloaded = poll_until(|| !model.is_loaded(), Duration::from_secs(2));
        assert!(unloaded, "watcher failed to unload");

        // Next call lazy-reloads → fresh generation.
        let g2 = model.with_model(|m| Ok(m.generation)).unwrap();
        assert_eq!(g2, 2, "expected fresh load after unload");
        assert!(model.is_loaded());
    }

    #[test]
    fn activity_extends_timer() {
        let model =
            IdleUnloadingModel::<FakeModel>::new(fake_loader(), Some(Duration::from_millis(300)));
        model.with_model(|_| Ok(())).unwrap();

        // Bump activity every 80 ms for ~600 ms (> timeout). Model must stay
        // loaded the whole time because the watcher never sees the window
        // elapse.
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(600) {
            model.note_activity();
            assert!(
                model.is_loaded(),
                "model unloaded mid-activity at {:?}",
                start.elapsed()
            );
            thread::sleep(Duration::from_millis(80));
        }

        // Stop bumping → eventually unloads.
        let unloaded = poll_until(|| !model.is_loaded(), Duration::from_secs(2));
        assert!(
            unloaded,
            "model failed to unload after activity stream ended"
        );
    }

    #[test]
    fn loader_error_leaves_slot_empty_and_is_retryable() {
        // First call fails; second call (after we flip the toggle) succeeds.
        let fail = Arc::new(AtomicBool::new(true));
        let fail2 = Arc::clone(&fail);
        let model = IdleUnloadingModel::<FakeModel>::new(
            move || {
                if fail2.load(Ordering::SeqCst) {
                    Err(anyhow!("synthetic loader failure"))
                } else {
                    Ok(FakeModel { generation: 7 })
                }
            },
            None,
        );

        let err = model.with_model(|_| Ok(())).unwrap_err();
        assert!(
            err.to_string().contains("lazy-load") || err.to_string().contains("synthetic"),
            "expected load-failure message, got: {err}"
        );
        assert!(!model.is_loaded(), "slot must be empty after loader error");

        fail.store(false, Ordering::SeqCst);
        let g = model.with_model(|m| Ok(m.generation)).unwrap();
        assert_eq!(g, 7, "retry should succeed once loader stops failing");
        assert!(model.is_loaded());
    }

    #[test]
    fn explicit_unload_drops_model() {
        let model = IdleUnloadingModel::<FakeModel>::new(fake_loader(), None);
        model.with_model(|_| Ok(())).unwrap();
        assert!(model.is_loaded());

        model.unload();
        assert!(!model.is_loaded(), "unload() did not drop the model");

        let g = model.with_model(|m| Ok(m.generation)).unwrap();
        assert_eq!(g, 2, "next call should lazy-reload after explicit unload");
    }

    #[test]
    fn idle_timeout_accessor_returns_configured_value() {
        let m1 = IdleUnloadingModel::<FakeModel>::new(fake_loader(), None);
        assert_eq!(m1.idle_timeout(), None);
        let m2 = IdleUnloadingModel::<FakeModel>::new(fake_loader(), Some(Duration::from_secs(30)));
        assert_eq!(m2.idle_timeout(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn poisoned_mutex_recovers_on_next_call() {
        // A panic inside the callback poisons the lock. The next call must
        // still find the model rather than refusing forever.
        let model = IdleUnloadingModel::<FakeModel>::new(fake_loader(), None);

        let model_arc = Arc::new(model);
        let model_for_thread = Arc::clone(&model_arc);
        // Run the panicking call on a worker thread so this test thread
        // doesn't itself unwind.
        let h = thread::spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                model_for_thread
                    .with_model(|_| -> Result<()> { panic!("synthetic panic inside callback") })
                    .unwrap()
            }));
        });
        h.join().unwrap();

        // The model was loaded before the panic; is_loaded must still report
        // true (poison recovery in the accessor) and a subsequent call must
        // succeed without reloading.
        assert!(
            model_arc.is_loaded(),
            "is_loaded() lost the model after mutex poison"
        );
        let g = model_arc.with_model(|m| Ok(m.generation)).unwrap();
        assert_eq!(g, 1, "callback after poison should see the same model");
    }

    #[test]
    fn load_guard_clears_loading_flag_on_panic() {
        // Direct test of the RAII discipline: even when the loader panics,
        // the `loading` flag returns to false (so the watcher isn't stuck
        // refusing to unload). We run the panicking load on a sub-thread,
        // catch its unwind, then read the flag.
        let shared: Arc<Shared<FakeModel>> = Arc::new(Shared::new());
        let shared_clone = Arc::clone(&shared);
        let h = thread::spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _g = LoadGuard::new(&shared_clone);
                panic!("synthetic loader panic");
            }));
        });
        h.join().unwrap();

        assert!(
            !shared.loading.load(Ordering::Acquire),
            "LoadGuard failed to clear loading flag on panic"
        );
    }

    #[test]
    fn drop_cleans_up_watcher_thread() {
        // A short-timeout wrapper that is immediately dropped must not leak
        // its watcher thread. We can't directly observe thread termination,
        // but `Drop` joins the watcher — if `shutdown` weren't signalled
        // the join would hang forever and the test would time out.
        let model =
            IdleUnloadingModel::<FakeModel>::new(fake_loader(), Some(Duration::from_millis(50)));
        // Give the watcher a tick to start.
        thread::sleep(Duration::from_millis(50));
        drop(model);
        // If we reach here without hanging, the watcher joined cleanly.
    }
}
