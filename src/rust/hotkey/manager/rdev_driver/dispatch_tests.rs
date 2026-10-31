//! Dispatch-slot and start-once regression tests for the rdev listener.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use super::{NativeListener, SinkSlot};
use crate::hotkey::manager::driver_common::SpawnError;

fn recording_sink(seen: &Arc<Mutex<Vec<u32>>>) -> Arc<dyn Fn(u32) + Send + Sync> {
    let seen = Arc::clone(seen);
    Arc::new(move |event| seen.lock().unwrap().push(event))
}

#[test]
fn reinstall_routes_events_only_to_the_newest_sink_and_clear_makes_dispatch_a_noop() {
    let slot: SinkSlot<u32> = SinkSlot::new();
    let seen_a = Arc::new(Mutex::new(Vec::new()));
    let seen_b = Arc::new(Mutex::new(Vec::new()));

    let token_a = slot.install(recording_sink(&seen_a));
    let token_b = slot.install(recording_sink(&seen_b));
    slot.dispatch(7);

    assert!(
        seen_a.lock().unwrap().is_empty(),
        "replaced sink must not see events"
    );
    assert_eq!(
        *seen_b.lock().unwrap(),
        vec![7],
        "newest sink sees the event once"
    );

    // A late teardown of the replaced install must not clear the live sink.
    slot.clear(token_a);
    slot.dispatch(8);
    assert_eq!(*seen_b.lock().unwrap(), vec![7, 8]);

    slot.clear(token_b);
    slot.dispatch(9);
    assert!(seen_a.lock().unwrap().is_empty());
    assert_eq!(
        *seen_b.lock().unwrap(),
        vec![7, 8],
        "dispatch after teardown must be a no-op"
    );
}

/// Keeps a fake listener thread alive until dropped.
struct FakeListener {
    release: Option<mpsc::Sender<()>>,
    join: Option<thread::JoinHandle<()>>,
}

impl FakeListener {
    fn stop(&mut self) {
        self.release.take();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}

fn fake_start(
    calls: &Arc<AtomicUsize>,
    stoppers: &Arc<Mutex<Vec<mpsc::Sender<()>>>>,
) -> impl FnOnce(Arc<AtomicBool>) -> (Option<thread::JoinHandle<()>>, Result<(), SpawnError>) {
    let calls = Arc::clone(calls);
    let stoppers = Arc::clone(stoppers);
    move |alive| {
        calls.fetch_add(1, Ordering::SeqCst);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        stoppers.lock().unwrap().push(release_tx);
        alive.store(true, Ordering::SeqCst);
        let join = thread::spawn(move || {
            let _ = release_rx.recv();
            alive.store(false, Ordering::SeqCst);
        });
        (Some(join), Ok(()))
    }
}

#[test]
fn listener_is_started_once_across_repeated_installs() {
    let listener = NativeListener::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let stoppers = Arc::new(Mutex::new(Vec::new()));

    for _ in 0..3 {
        listener
            .ensure_started(fake_start(&calls, &stoppers))
            .expect("running listener is reused");
    }

    assert_eq!(calls.load(Ordering::SeqCst), 1, "listener must start once");
    assert!(listener.alive_flag().load(Ordering::SeqCst));

    let mut running = FakeListener {
        release: stoppers.lock().unwrap().pop(),
        join: listener.thread.lock().unwrap().take(),
    };
    running.stop();
}

#[test]
fn listener_restarts_only_after_the_previous_thread_exited() {
    let listener = NativeListener::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let stoppers = Arc::new(Mutex::new(Vec::new()));

    listener
        .ensure_started(fake_start(&calls, &stoppers))
        .unwrap();
    // Let the first listener thread exit, as rdev::listen does on failure.
    stoppers.lock().unwrap().clear();
    while !listener
        .thread
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|t| t.is_finished())
    {
        thread::yield_now();
    }

    listener
        .ensure_started(fake_start(&calls, &stoppers))
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let mut running = FakeListener {
        release: stoppers.lock().unwrap().pop(),
        join: listener.thread.lock().unwrap().take(),
    };
    running.stop();
}

#[test]
fn running_listener_that_never_reached_its_hook_reports_hung() {
    let listener = NativeListener::new();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let result = listener.ensure_started(|_alive| {
        let join = thread::spawn(move || {
            let _ = release_rx.recv();
        });
        (Some(join), Err(SpawnError::ListenerHung))
    });
    assert!(matches!(result, Err(SpawnError::ListenerHung)));

    let calls = Arc::new(AtomicUsize::new(0));
    let stoppers = Arc::new(Mutex::new(Vec::new()));
    let reuse = listener.ensure_started(fake_start(&calls, &stoppers));
    assert!(matches!(reuse, Err(SpawnError::ListenerHung)));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a still-running listener thread must not be duplicated"
    );

    let mut running = FakeListener {
        release: Some(release_tx),
        join: listener.thread.lock().unwrap().take(),
    };
    running.stop();
}
