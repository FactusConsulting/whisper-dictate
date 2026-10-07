//! Companion tests for the tracker-bridge decision
//! ([`super::bridge_decision`]).
//!
//! Covers the PTT gate the runtime installs while a paste-last burst is
//! in flight and the one-shot
//! action sink forwarding.

#![cfg(all(test, feature = "rust-hotkeys"))]

use super::install::bridge_decision;
use super::HotkeyActionSinks;
use crate::hotkey::coordinator::CoordinatorEvent;
use crate::hotkey::manager::TrackerOutput;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

#[test]
fn paste_burst_gate_drops_ptt_presses_until_cleared() {
    // The runtime gates the press while a paste-last burst is in flight:
    // a recording accepted mid-burst would be corrupted by the worker's
    // held-modifier release and the remaining keystrokes landing under
    // the new recording's modifiers.
    let gated = HotkeyActionSinks {
        ptt_gate: Some(Arc::new(|| true)),
        ..Default::default()
    };
    assert!(bridge_decision(TrackerOutput::ChordPress, &gated).is_none());
    // Releases are never gated: a release for an already-accepted press
    // must still reach the coordinator.
    assert!(matches!(
        bridge_decision(TrackerOutput::ChordRelease, &gated),
        Some(CoordinatorEvent::Release)
    ));
    assert!(matches!(
        bridge_decision(TrackerOutput::ChordCancel, &gated),
        Some(CoordinatorEvent::Cancel)
    ));

    // An open gate (or no gate at all) leaves presses flowing.
    let open = HotkeyActionSinks {
        ptt_gate: Some(Arc::new(|| false)),
        ..Default::default()
    };
    assert!(matches!(
        bridge_decision(TrackerOutput::ChordPress, &open),
        Some(CoordinatorEvent::Press)
    ));
    let plain = HotkeyActionSinks::default();
    assert!(matches!(
        bridge_decision(TrackerOutput::ChordPress, &plain),
        Some(CoordinatorEvent::Press)
    ));
}

#[test]
fn bridge_fires_one_shot_sinks_and_drops_their_events() {
    let fired = Arc::new(AtomicBool::new(false));
    let saw = Arc::clone(&fired);
    let sinks = HotkeyActionSinks {
        copy_last: Arc::new(move || saw.store(true, Ordering::Release)),
        ..Default::default()
    };
    // The action fires exactly once and its output is never forwarded to
    // the coordinator.
    assert!(bridge_decision(TrackerOutput::CopyLast, &sinks).is_none());
    assert!(fired.load(Ordering::Acquire));
    assert!(bridge_decision(TrackerOutput::PasteLast, &sinks).is_none());
}

#[test]
fn bridge_fires_mode_shortcut_sinks_without_coordinator_events() {
    // Each mode shortcut advances its own counter exactly once; none of
    // the three outputs may reach the coordinator (they never start or
    // stop a recording).
    let fired = Arc::new(AtomicU8::new(0));
    let cycle_saw = Arc::clone(&fired);
    let raw_saw = Arc::clone(&fired);
    let clean_saw = Arc::clone(&fired);
    let sinks = HotkeyActionSinks {
        cycle_mode: Arc::new(move || {
            cycle_saw.fetch_add(1, Ordering::Release);
        }),
        raw_mode: Arc::new(move || {
            raw_saw.fetch_add(2, Ordering::Release);
        }),
        clean_mode: Arc::new(move || {
            clean_saw.fetch_add(4, Ordering::Release);
        }),
        ..Default::default()
    };
    assert!(bridge_decision(TrackerOutput::CycleMode, &sinks).is_none());
    assert!(bridge_decision(TrackerOutput::RawMode, &sinks).is_none());
    assert!(bridge_decision(TrackerOutput::CleanMode, &sinks).is_none());
    assert_eq!(fired.load(Ordering::Acquire), 7);
}
