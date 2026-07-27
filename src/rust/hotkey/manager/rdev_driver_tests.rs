//! Companion tests for [`crate::hotkey::manager::rdev_driver`].
//!
//! Extracted from an inline `#[cfg(test)] mod tests` in `rdev_driver.rs`
//! so the regression-test discipline scanner (per AGENTS.md
//! `enforce-regression-test-discipline` — see
//! `src/tests/python/test_regression_test_discipline.py`) sees a matching
//! test file next to the production module. The inline layout is not
//! picked up by the scanner's "already-tested" exemption, which resolves
//! `foo.rs` → `foo_tests.rs` on the file system.
//!
//! Every test here is `#[cfg(feature = "rust-hotkeys")]` because the
//! production module is itself feature-gated; on a stock build the whole
//! file compiles to nothing and the test harness sees zero tests.

#![cfg(all(test, feature = "rust-hotkeys"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::hotkey::inject_guard::InjectionGuard;
use crate::hotkey::manager::rdev_driver::{
    is_rdev_supported_name, raw_from_rdev, should_log_raw_event, spawn, SpawnError,
};
use crate::hotkey::manager::tracker::RawKeyKind;

#[test]
fn register_and_unregister_roundtrip() {
    // Lightweight test that register/unregister responses come back
    // through the mpsc — does NOT exercise the rdev listener thread
    // (it's still installed but no synthetic events are injected).
    // In headless CI / containers rdev::listen returns Err immediately
    // (no X display / no accessibility permission) — that's exactly
    // the P1-#2 startup-failure path, so we skip the round-trip on
    // such platforms rather than assert success.
    let count = Arc::new(AtomicUsize::new(0));
    let count_cb = Arc::clone(&count);
    let guard = Arc::new(InjectionGuard::new());
    let (handle, _thread) = match spawn(guard, move |_out| {
        count_cb.fetch_add(1, Ordering::SeqCst);
    }) {
        Ok(pair) => pair,
        Err(SpawnError::ListenerStartup(_)) | Err(SpawnError::ListenerHung) => {
            eprintln!(
                "skipping register_and_unregister_roundtrip: rdev listener \
                 refused to start (headless env)"
            );
            return;
        }
    };
    handle
        .register(vec!["ctrl_l".to_owned(), "f9".to_owned()])
        .expect("register");
    handle.unregister().expect("unregister");
    handle
        .register(vec!["shift_r".to_owned()])
        .expect("re-register");
    // No events fired through the tracker — count stays zero.
    assert_eq!(count.load(Ordering::SeqCst), 0);
    handle.shutdown();
    // Do NOT join: the rdev listener thread is unjoinable, but the
    // manager thread is — drop the handle and let the test runner
    // finish. (The thread exits on its own when it sees Shutdown.)
}

#[test]
fn listener_startup_failure_is_surfaced_to_caller() {
    // On a headless Linux container rdev::listen returns Err very
    // quickly (no X display). The driver MUST propagate that to the
    // spawn-side caller instead of silently logging and exiting, so
    // the supervisor can keep the Python listener wired (P1 #2).
    // We don't have a way to force the failure on platforms where the
    // hook genuinely works, so on those we treat success as "test not
    // applicable" rather than fail.
    let guard = Arc::new(InjectionGuard::new());
    match spawn(guard, |_out| {}) {
        Ok((handle, _thread)) => {
            handle.shutdown();
        }
        Err(SpawnError::ListenerStartup(msg)) => {
            assert!(
                !msg.is_empty(),
                "ListenerStartup error message should not be empty"
            );
        }
        Err(SpawnError::ListenerHung) => {
            // Hung is also a "tell the caller" outcome — acceptable.
        }
    }
}

#[test]
fn rdev_name_set_covers_every_emitted_key() {
    // Every name the rdev->name mapping can emit must appear in the
    // supported-names set so the install-time validator never rejects
    // a name we DO support. If you add a key in `key_to_name`, add it
    // to `RDEV_SUPPORTED_NAMES` (and adjust this assertion if you also
    // expose a new bare-modifier alias).
    for key in [
        rdev::Key::ControlLeft,
        rdev::Key::ControlRight,
        rdev::Key::ShiftLeft,
        rdev::Key::ShiftRight,
        rdev::Key::Alt,
        rdev::Key::AltGr,
        rdev::Key::MetaLeft,
        rdev::Key::MetaRight,
        rdev::Key::F1,
        rdev::Key::F12,
        rdev::Key::Space,
        rdev::Key::Escape,
        rdev::Key::Tab,
        rdev::Key::Return,
    ] {
        let ev = rdev::Event {
            event_type: rdev::EventType::KeyPress(key),
            time: std::time::SystemTime::UNIX_EPOCH,
            name: None,
        };
        let raw = raw_from_rdev(&ev).expect("mapped or synthetic name");
        // Reject synthetic names for known keys — they must be in the
        // real map, otherwise `is_rdev_supported_name` will not accept them.
        assert!(
            !raw.name.starts_with("__rdev_"),
            "key {key:?} produced synthetic name {name}; add it to key_to_name",
            name = raw.name
        );
        assert!(
            is_rdev_supported_name(&raw.name),
            "rdev emits {name} but install-time validator rejects it",
            name = raw.name
        );
    }
}

#[test]
fn unsupported_names_are_rejected_by_validator() {
    // Names accepted by the Python evdev/pynput backends but NOT by the
    // rdev driver. Without the validator a configuration that contains
    // any of these would install successfully but never fire (P2 #6).
    for name in ["super_l", "super_r", "menu", "scroll_lock", "pause"] {
        assert!(
            !is_rdev_supported_name(name),
            "rdev driver claims to support {name} — update the test or the map",
        );
    }
}

// -----------------------------------------------------------------------
// P2 #346 finding 4: right_alt / ralt aliases.
// -----------------------------------------------------------------------

#[test]
fn right_alt_and_ralt_aliases_are_accepted_by_validator() {
    // Users and documentation sometimes refer to AltGr as "right_alt"
    // or "ralt". The install-time validator must accept these so the
    // Rust backend doesn't reject a valid AltGr PTT binding.
    for name in ["right_alt", "ralt"] {
        assert!(
            is_rdev_supported_name(name),
            "{name} should be accepted as an AltGr alias (P2 #346 finding 4)",
        );
    }
}

// -----------------------------------------------------------------------
// P2 #346 finding 2: unmapped (ordinary) keys reach the tracker.
// -----------------------------------------------------------------------

#[test]
fn raw_from_rdev_produces_event_for_unmapped_key() {
    // Keys not in key_to_name (e.g. letter keys) must still produce a
    // RawKeyEvent so the tracker can detect foreign-key holds and emit
    // ChordCancel for bare-modifier bindings (rule 2). Previously
    // raw_from_rdev returned None for these, silently dropping them.
    use rdev::{Event, EventType};

    let press_a = Event {
        event_type: EventType::KeyPress(rdev::Key::KeyA),
        time: std::time::SystemTime::UNIX_EPOCH,
        name: None,
    };
    let raw = raw_from_rdev(&press_a);
    assert!(
        raw.is_some(),
        "ordinary key press must produce a RawKeyEvent for foreign-key tracking"
    );
    let raw = raw.unwrap();
    assert!(
        raw.name.starts_with("__rdev_"),
        "unmapped key should use synthetic __rdev_ name, got {:?}",
        raw.name
    );
    assert_eq!(raw.kind, RawKeyKind::Press);
}

// -----------------------------------------------------------------------
// Windows PTT wedge diagnostic: rate-limit for the per-event trace line.
//
// The rdev listener sees EVERY desktop-wide keydown/keyup on Windows —
// logging each one would flood the diagnostic file (which is `append`
// mode across sessions) and slow the LL-hook thread enough to skew the
// very timing we are trying to measure. The pure `should_log_raw_event`
// helper decides which event indices actually emit a line. Keep the
// first ten (so the user's first key press after startup always shows
// up) and then every 100th (so we still see forward progress in long
// sessions). This is testable without spawning any threads or the OS
// listener; the runtime just consults it per event.
// -----------------------------------------------------------------------

#[test]
fn should_log_raw_event_prints_first_ten_events() {
    // First ten events (1..=10) always log — otherwise a user who presses
    // ctrl_l once at startup and reports "no events" would leave the log
    // silent, and we'd be unable to tell "hook installed but pump idle"
    // apart from "hook installed but rate-limit ate the trace".
    for n in 1..=10 {
        assert!(
            should_log_raw_event(n),
            "should_log_raw_event({n}) must be true — first ten events \
             always log so an early press is always visible"
        );
    }
}

#[test]
fn should_log_raw_event_skips_events_11_through_99() {
    // After the first ten and before the 100-mark, we suppress every
    // event to keep the file small. A regression that flipped this would
    // flood the diagnostic file on every burst of typing.
    for n in 11..100 {
        assert!(
            !should_log_raw_event(n),
            "should_log_raw_event({n}) must be false — events 11..100 \
             are suppressed by the rate limit"
        );
    }
}

#[test]
fn should_log_raw_event_prints_every_hundredth_thereafter() {
    // 100, 200, 300, ... always log. This proves forward progress in
    // long sessions without flooding.
    for k in 1..=5 {
        let n = 100 * k;
        assert!(
            should_log_raw_event(n),
            "should_log_raw_event({n}) must be true — multiples of 100 \
             log so long sessions show forward progress"
        );
    }
    // And a value between two multiples of 100 does NOT log — we're
    // sampling, not summing.
    assert!(
        !should_log_raw_event(150),
        "should_log_raw_event(150) must be false — only exact multiples \
         of 100 satisfy the every-100th rule"
    );
}

#[test]
fn should_log_raw_event_zero_is_never_a_valid_index() {
    // Counter is 1-indexed (we `fetch_add(1)` then read the returned
    // value plus one), so n=0 should never be passed. If it ever is,
    // suppress rather than log — a stray 0 would make the log file
    // depend on unrelated startup ordering.
    assert!(
        !should_log_raw_event(0),
        "should_log_raw_event(0) must be false — the counter is 1-indexed \
         and 0 indicates a caller bug"
    );
}
