//! Feature-gated OS-event collection, kept separate from capture protocol.

use super::actions::Counters;
use super::CaptureEvent;
#[cfg(feature = "rust-hotkeys")]
use crate::hotkey::manager::{is_chord_key, RawKeyEvent};
#[cfg(feature = "rust-hotkeys")]
use std::collections::HashSet;
#[cfg(feature = "rust-hotkeys")]
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
#[cfg(feature = "rust-hotkeys")]
use std::sync::Mutex;
use std::time::Instant;

#[cfg(feature = "rust-hotkeys")]
pub(super) fn build_raw_tap(
    counters: Arc<Counters>,
    tx: Sender<CaptureEvent>,
    start: Instant,
    targets: Vec<String>,
    emit_raw_events: bool,
) -> impl crate::hotkey::manager::RawTap {
    // Foreign keys are counted as PHYSICAL presses, not raw key-down events.
    // Holding a non-chord key produces a stream of auto-repeat key-downs (the
    // maintainer's capture logged ~20 for a single held Shift), so counting
    // events would report 20 foreign keys for one keypress. Track which
    // foreign keys are currently down and count only the not-held -> held
    // transition.
    //
    // `RawTap` is `Fn + Send + Sync`, so the held-set needs interior
    // mutability; the lock is uncontended (one listener thread) and held for
    // a single set operation.
    let held_foreign: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    move |raw: &RawKeyEvent| {
        counters.events.fetch_add(1, Ordering::Relaxed);
        let is_foreign = !is_chord_key(&targets, &raw.name);
        let t_secs = start.elapsed().as_secs_f64();
        let event = match raw.kind {
            crate::hotkey::manager::RawKeyKind::Press => {
                if is_foreign {
                    // `insert` returns false when the key was already held,
                    // which is exactly the auto-repeat case.
                    if let Ok(mut held) = held_foreign.lock() {
                        if held.insert(raw.name.clone()) {
                            counters.foreign_keys.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                CaptureEvent::KeyDown {
                    t_secs,
                    name: raw.name.clone(),
                }
            }
            crate::hotkey::manager::RawKeyKind::Release => {
                if is_foreign {
                    if let Ok(mut held) = held_foreign.lock() {
                        held.remove(&raw.name);
                    }
                }
                CaptureEvent::KeyUp {
                    t_secs,
                    name: raw.name.clone(),
                }
            }
        };
        if emit_raw_events {
            let _ = tx.send(event);
        }
    }
}

/// Non-feature build: the tap is never invoked (install returns Unsupported
/// before threads spawn), so return a zero-cost noop. Kept here so
/// `run_capture` compiles under both feature configurations.
///
/// Signature MUST match the feature-gated version — the caller in
/// `run_capture` passes `key_names.clone()` as the fourth argument
/// unconditionally, so this stub takes (and ignores) `_targets` too. Skipping
/// it broke the stock (`--no-default-features` / no `rust-hotkeys`) build with
/// an E0061 argument-count error while the feature build stayed green.
#[cfg(not(feature = "rust-hotkeys"))]
#[allow(clippy::unused_unit)]
pub(super) fn build_raw_tap(
    _counters: Arc<Counters>,
    _tx: Sender<CaptureEvent>,
    _start: Instant,
    _targets: Vec<String>,
    _emit_raw_events: bool,
) -> impl Send + Sync + 'static {
    // `()` implements Send + Sync + 'static and satisfies the stock-build
    // `install_hotkey_with_focus_snapshot` bound. It is never invoked — the stock
    // install returns Unsupported before any listener thread spawns.
    ()
}

#[cfg(all(test, feature = "rust-hotkeys"))]
#[path = "raw_tap_tests.rs"]
mod tests;
