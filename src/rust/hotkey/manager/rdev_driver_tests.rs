//! Companion tests for [`crate::hotkey::manager::rdev_driver`].
//!
//! Extracted from an inline `#[cfg(test)] mod tests` in `rdev_driver.rs`
//! so the regression-test discipline scanner (per AGENTS.md
//! `enforce-regression-test-discipline`) sees a matching
//! test file next to the production module. The inline layout is not
//! picked up by the scanner's "already-tested" exemption, which resolves
//! `foo.rs` → `foo_tests.rs` on the file system.
//!
//! Every test here is `#[cfg(feature = "rust-hotkeys")]` because the
//! production module is itself feature-gated; on a stock build the whole
//! file compiles to nothing and the test harness sees zero tests.

#![cfg(all(test, feature = "rust-hotkeys"))]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, MutexGuard};

use crate::diag_test_lock::DIAG_WRITER_LOCK;
use crate::hotkey::inject_guard::InjectionGuard;
use crate::hotkey::manager::rdev_driver::{
    // Merged when rebasing #668 onto main. KEEP BOTH sides:
    // * from main (#673): `spawn_heartbeat_thread*` +
    //    `spawn_with_raw_tap_capturing_heartbeat_for_tests` +
    //    `NoopRawTap`, used by the heartbeat-exit assertions.
    // * from main (#665): `redact_event_type_for_debug`, used by the
    //    debug pre-filter redaction test.
    //  * from #668: the off-callback queue helpers
    //    (`enqueue_callback_trace`,
    //    `ensure_callback_trace_writer_for_tests`,
    //    `CALLBACK_TRACE_QUEUE_CAPACITY`).
    await_listener_ready,
    enqueue_callback_trace,
    ensure_callback_trace_writer_for_tests,
    is_rdev_supported_name,
    listener_readiness_handshake,
    raw_from_rdev,
    redact_event_type_for_debug,
    redact_raw_event_name,
    should_log_raw_event,
    spawn,
    spawn_heartbeat_thread,
    spawn_heartbeat_thread_with_config,
    spawn_with_raw_tap_capturing_heartbeat_for_tests,
    HeartbeatState,
    ListenerSignal,
    ListenerStart,
    NoopRawTap,
    SpawnError,
    CALLBACK_TRACE_QUEUE_CAPACITY,
    HEARTBEAT_HEALTHY_QUOTA,
    HEARTBEAT_IDLE_EMIT_EVERY,
};
use crate::hotkey::manager::tracker::RawKeyKind;

/// Take the crate-wide [`DIAG_WRITER_LOCK`].
///
/// The callback-queue flood tests below push thousands of lines
/// through the SAME process-wide async queue and writer thread that
/// `crate::diag`'s tee file sits behind. Without this lock they race
/// the diagnostic-log tests in `diag_tests` and `tracker_tests` two
/// ways (#668 3666690064):
///
///  1. The queue is a bounded `sync_channel` — a flood can fill it so
///     a concurrent tracker test's `[chord]` line is silently dropped
///     by `try_send`, and its `contains("[chord]")` assertion fails.
///  2. `flush_async_for_tests` waits for the pending-count to reach
///     zero; a concurrent flood keeps it non-zero, so the waiter times
///     out and reads the tee file before the line it needs has landed.
///
/// Flood lines can also be written into whichever tempfile the other
/// suite installed, which is harmless for today's assertions but is
/// exactly the cross-test bleed the lock exists to prevent.
///
/// Poison-tolerant (`into_inner`) so one failing test does not cascade
/// into unrelated failures across the suite — same shape as the
/// helpers in `diag_tests` and `tracker_tests`.
fn diag_test_lock() -> MutexGuard<'static, ()> {
    DIAG_WRITER_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

#[path = "rdev_driver/keys_tests.rs"]
mod keys;

#[path = "rdev_driver/diagnostics_tests.rs"]
mod diagnostics;

#[path = "rdev_driver/heartbeat_tests.rs"]
mod heartbeat;

#[path = "rdev_driver/readiness_tests.rs"]
mod readiness;

#[path = "rdev_driver/listener_tests.rs"]
mod listener;

#[path = "rdev_driver/listener_thread_tests.rs"]
mod listener_thread;
