//! Iteration-2 review finding #3 regression tests.
//!
//! When the audio bridge surfaces a terminal `BridgeError::Io` or
//! `BridgeError::Pipeline`, the writer thread has already dropped the
//! child's stdin handle and exited. The supervisor MUST treat that as
//! a fatal worker failure — kill the child, drop the bridge, surface
//! an `Exited` event so the UI flips to Stopped — instead of leaving
//! a half-dead worker that can no longer receive audio.
//!
//! The watcher itself runs on a background thread and cannot mutate
//! `&mut self`; the contract is "watcher raises a flag, next `poll()`
//! performs the teardown". These tests pin that contract by toggling
//! the flag via the crate-visible test hook and then asserting `poll`
//! does the right thing.
//!
//! Only built with the `audio-in-rust` cargo feature because the flag
//! itself only exists in that build (it's `#[cfg(feature = "audio-in-rust")]`).
//!
//! **Wave 8 Part 2**: the "with a running child" variant
//! (`terminal_bridge_flag_kills_child_and_surfaces_exited_on_next_poll`)
//! spawned a `python -c "..."` supervisor child through the
//! pre-Wave-8 `WorkerCommand`. The supervisor now UNCONDITIONALLY
//! swaps program+args to `<current-exe> worker-rust` and requires a
//! resolvable GGML model before it will spawn anything at all, so the
//! test can no longer prime a live worker to tear down. The
//! iteration-2 finding #3 teardown logic itself (kill child on flag +
//! synthesize Exited) is unchanged; the pre-start no-child variant
//! below still pins the safe-idle branch of the same code path.

#![cfg(feature = "audio-in-rust")]

use super::*;
use crate::runtime::test_support::ENV_LOCK;

#[test]
fn terminal_bridge_flag_without_child_just_resets_state() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Pre-start state. The watcher can race against an already-exited
    // worker (child died → poll() saw it first → next iteration sees
    // the bridge flag). The teardown branch must be safe in that case
    // — no panic, no spurious extra Exited event, state stays Stopped.
    let mut supervisor = RuntimeSupervisor::new();
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
    supervisor.trigger_bridge_terminal_for_tests();
    let events = supervisor.poll();
    assert_eq!(
        supervisor.state(),
        RuntimeState::Stopped,
        "state must stay Stopped when there is no child to kill",
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::Exited { .. })),
        "finding #3: no spurious Exited when there was no child to begin with; got: {events:?}",
    );
}
