//! Lifecycle tests for the public [`IdleUnloadingModel`] wrapper.
//!
//! Lives in its own file so the production wrapper module stays under the
//! repo's ~500-line modularity gate. Uses a `FakeModel` so the tests cover
//! load → idle → unload → reload, activity-extension, panic recovery, and
//! teardown without needing a real 75 MB GGML file. Pure unit tests for
//! the env parser live in `env.rs`; tests for the unload-decision helper
//! live in `watcher.rs`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use super::watcher::LoadGuard;
use super::{IdleUnloadingModel, Shared};

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
