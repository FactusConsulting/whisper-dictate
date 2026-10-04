//! Companion tests for the tracker-bridge decision
//! ([`super::bridge_decision`]).
//!
//! Covers the PTT gate the runtime installs while a paste-last burst is
//! in flight (Codex P2 win_registerhotkey.rs:383) and the one-shot
//! action sink forwarding.

#![cfg(all(test, feature = "rust-hotkeys"))]

use super::{bridge_decision, HotkeyActionSinks};
use crate::hotkey::coordinator::CoordinatorEvent;
use crate::hotkey::manager::TrackerOutput;
use std::sync::atomic::{AtomicBool, Ordering};
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
