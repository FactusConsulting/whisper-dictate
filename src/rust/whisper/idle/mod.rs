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
//! - **RAII load discipline.** [`LoadGuard`](watcher::LoadGuard) enforces
//!   that the "currently loading" book-keeping is reset on every exit path
//!   — Ok, Err, or panic. That keeps `is_loaded()` truthful even if the
//!   loader itself panics.
//!
//! The wrapper is **generic over the model type** so the unit tests can
//! exercise the lifecycle (load → idle → unload → reload, activity-extension,
//! poison recovery) without needing a real 75 MB GGML file at test time —
//! see the sibling `tests` submodule for the `FakeModel` lifecycle tests.
//!
//! This module deliberately does not wire itself into the runtime: the
//! `whisper-rs-local` subprocess dispatcher (`whisper::dispatch`) spawns a
//! fresh process per transcribe and therefore cannot benefit from in-process
//! caching. The wrapper is the library primitive a future in-process worker
//! will reach for; landing it now (per the Wave 7-A roadmap entry) lets the
//! later wiring PR stay tiny and focused.
//!
//! ## Module layout
//!
//! The implementation is split across small files so each piece stays under
//! the repo's ~500-line modularity gate and remains independently
//! unit-testable:
//!
//! - [`env`] — env-var parsing and the [`IDLE_UNLOAD_ENV`] constant.
//! - [`watcher`] — background watcher loop, the unload-decision helper, and
//!   the `LoadGuard` RAII type.
//! - [`tests`] (test-only) — lifecycle tests against a `FakeModel`.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::whisper::LocalWhisper;

mod env;
mod watcher;

#[cfg(test)]
mod tests;

pub use env::{parse_idle_timeout_from_env, IDLE_UNLOAD_ENV};

use watcher::{now_ms, watcher_loop, LoadGuard};

/// Shared mutable state between the public handle and the watcher thread.
///
/// Lives behind an `Arc` so the watcher (which outlives `IdleUnloadingModel`
/// only briefly, during shutdown join) keeps a stable reference.
pub(super) struct Shared<M> {
    /// The model itself. `None` means "unloaded — reload on demand". A
    /// poisoned lock is recovered via `into_inner` because the model itself
    /// is immutable from our perspective and a panic inside a user callback
    /// hasn't corrupted it.
    pub(super) model: Mutex<Option<M>>,
    /// Last-activity timestamp as Unix epoch milliseconds. We use `i64`
    /// (signed) so the rare pre-epoch system clock state doesn't underflow
    /// silently; the watcher tolerates any value because it only compares
    /// against `now()`.
    pub(super) last_activity_ms: AtomicI64,
    /// `true` while a load is in progress. Prevents the watcher from racing
    /// the loader to drop the slot before it gets populated. The
    /// [`LoadGuard`] RAII handle is the only thing that toggles this.
    pub(super) loading: AtomicBool,
}

impl<M> Shared<M> {
    pub(super) fn new() -> Self {
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
