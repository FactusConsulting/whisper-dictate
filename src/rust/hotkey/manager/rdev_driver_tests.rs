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
    is_rdev_supported_name, raw_from_rdev, redact_event_type_for_debug, redact_raw_event_name,
    should_log_raw_event, spawn, HeartbeatState, SpawnError, HEARTBEAT_HEALTHY_QUOTA,
    HEARTBEAT_IDLE_EMIT_EVERY,
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
// Codex P2 #656 r3663653258 — rdev fallback must accept every
// side-specific alias that `parse_chord` rejects on the RegisterHotKey
// path. Without this, `win_l+f9`, `win_r+f9`, and `alt_r+f9` bindings
// installed the RegisterHotKey backend, were rejected as side-specific,
// and then hit `UnsupportedKey` on the promised rdev fallback.
// -----------------------------------------------------------------------

#[test]
fn side_specific_aliases_rejected_by_register_are_accepted_by_rdev() {
    for name in ["alt_r", "win_l", "win_r", "win"] {
        assert!(
            is_rdev_supported_name(name),
            "{name} must be accepted by the rdev install-time validator so \
             the RegisterHotKey→rdev fallback works (Codex P2 #656 r3663653258)",
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

// -----------------------------------------------------------------------
// Codex P1 #646 r3661145597 — redact non-PTT global keystrokes.
// -----------------------------------------------------------------------

#[test]
fn redact_raw_event_name_hides_unmapped_letters() {
    // The LL-hook callback observes every desktop-wide keydown/keyup on
    // Windows when rust-hotkeys is enabled. `raw_from_rdev` encodes each
    // unmapped alphanumeric / punctuation key as `__rdev_<Debug>`, so a
    // per-event trace samples ordinary typing well enough to reconstruct
    // password / token fragments. The redactor must return `<redacted>`
    // for anything not in the PTT-eligible name set so those samples
    // never land in gui-diagnostic.log.
    for name in [
        "__rdev_KeyA",
        "__rdev_Num5",
        "__rdev_Semicolon",
        "__rdev_KeyE",
        "__rdev_Slash",
        "a",
        "5",
        ";",
    ] {
        assert_eq!(
            redact_raw_event_name(name),
            "<redacted>",
            "non-PTT name {name} must be redacted"
        );
    }
}

#[test]
fn redact_raw_event_name_keeps_ptt_eligible_names_visible() {
    // The whole point of the trace line is to catch chord-matcher
    // rejections like "hook is alive but the event is called `ctrl` not
    // `ctrl_l`". PTT-eligible names (F-keys, modifier sides, space, esc,
    // tab, enter, AltGr aliases, generic modifiers) MUST pass through
    // verbatim — otherwise we would suppress the very diagnostic value
    // the trace exists for.
    for name in [
        "f9",
        "f1",
        "f12",
        "ctrl_l",
        "ctrl_r",
        "ctrl",
        "shift_l",
        "shift_r",
        "alt_l",
        "alt_gr",
        "cmd_l",
        "space",
        "esc",
        "tab",
        "enter",
        "ralt",
        "right_alt",
    ] {
        assert_eq!(
            redact_raw_event_name(name),
            name,
            "PTT-eligible name {name} must survive redaction verbatim"
        );
    }
}

// -----------------------------------------------------------------------
// Codex P1 #657 r3663766123 — redact pre-filter `[rdev/callback] raw=` trace.
//
// The debug-level pre-filter line runs on EVERY event rdev delivers
// (unsampled), so leaking the raw `Key` variant identity there
// defeats the sampled-line redaction below it. The redactor keeps
// PTT-eligible key names visible for the diagnostic use-case (F9
// rdev sees but key_to_name discards) and strips everything else.
// -----------------------------------------------------------------------

#[test]
fn redact_event_type_hides_ordinary_key_identity() {
    // Ordinary typing (letters/digits/punctuation) would leak
    // password/token fragments if `event_type` were emitted `{:?}` —
    // the plain Debug prints `KeyPress(KeyA)`, `KeyPress(Num5)`, etc.
    for key in [
        rdev::Key::KeyA,
        rdev::Key::KeyE,
        rdev::Key::Num5,
        rdev::Key::SemiColon,
        rdev::Key::Slash,
    ] {
        let press = rdev::EventType::KeyPress(key);
        let release = rdev::EventType::KeyRelease(key);
        assert_eq!(
            redact_event_type_for_debug(&press),
            "KeyPress(<redacted>)",
            "ordinary key {key:?} press must be redacted"
        );
        assert_eq!(
            redact_event_type_for_debug(&release),
            "KeyRelease(<redacted>)",
            "ordinary key {key:?} release must be redacted"
        );
    }
}

#[test]
fn redact_event_type_keeps_ptt_eligible_keys_visible() {
    // The whole diagnostic purpose of the `[rdev/callback] raw=` line
    // is to catch cases where rdev sees an F-key or modifier but
    // key_to_name discards it. PTT-eligible key events must survive
    // redaction verbatim so that value stays on the diagnostic.
    let cases: &[(rdev::Key, &str)] = &[
        (rdev::Key::F1, "KeyPress(f1)"),
        (rdev::Key::F9, "KeyPress(f9)"),
        (rdev::Key::F12, "KeyPress(f12)"),
        (rdev::Key::ControlLeft, "KeyPress(ctrl_l)"),
        (rdev::Key::ShiftRight, "KeyPress(shift_r)"),
        (rdev::Key::AltGr, "KeyPress(alt_gr)"),
        (rdev::Key::MetaLeft, "KeyPress(cmd_l)"),
        (rdev::Key::Space, "KeyPress(space)"),
        (rdev::Key::Escape, "KeyPress(esc)"),
    ];
    for (key, expected) in cases {
        let press = rdev::EventType::KeyPress(*key);
        assert_eq!(
            redact_event_type_for_debug(&press),
            *expected,
            "PTT-eligible key {key:?} press must render as {expected}"
        );
    }
}

#[test]
fn redact_event_type_passes_through_non_key_events() {
    // Mouse move / wheel / button events carry no keyboard identity;
    // their `{:?}` form is useful for diagnosing mouse-hook chain
    // interaction so they pass through unchanged.
    let mouse_move = rdev::EventType::MouseMove { x: 10.0, y: 20.0 };
    let s = redact_event_type_for_debug(&mouse_move);
    assert!(
        s.starts_with("MouseMove"),
        "non-key events must render as their Debug form, got {s:?}"
    );
}

// -----------------------------------------------------------------------
// Codex P2 #646 r3661145600 — heartbeat lifecycle on startup failure.
// -----------------------------------------------------------------------

#[test]
fn spawn_startup_failure_stops_heartbeat_thread() {
    // Best-effort: on a host where rdev::listen actually starts, the
    // returned Ok is out of scope for this test — we just require that
    // the code path exists. On headless CI (no display / no accessibility)
    // rdev::listen fails within READY_PROBE_WINDOW and `spawn` returns
    // ListenerStartup; the fix guarantees the heartbeat's stop atomic is
    // set before the error propagates, so a caller that retries does not
    // stack orphan threads. We can't observe the atomic from outside
    // `spawn` (it is created inside the function), but we CAN observe
    // that spawn does not leak forever — if it returned quickly with an
    // error, the leak-avoidance path was traversed.
    let guard = Arc::new(InjectionGuard::new());
    let start = std::time::Instant::now();
    match spawn(guard, |_out| {}) {
        Ok((handle, _thread)) => {
            handle.shutdown();
        }
        Err(SpawnError::ListenerStartup(_)) | Err(SpawnError::ListenerHung) => {
            // The startup failure returned in a bounded time — the
            // early-return path (which now sets heartbeat_stop first)
            // was taken. If it hadn't been, this call would still return
            // quickly but a heartbeat thread would keep writing forever;
            // the leak is not directly observable from this test, but
            // the paired `heartbeat_state_*` tests below pin the emit /
            // retire policy at the pure-decision layer.
        }
    }
    // Guard against a regression that makes `spawn` block forever after
    // a startup failure.
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "spawn must return promptly on startup failure"
    );
}

// -----------------------------------------------------------------------
// Codex P2 #646 r3661145603 — bounded heartbeat log growth.
// -----------------------------------------------------------------------

#[test]
fn heartbeat_state_emits_and_never_retires_during_a_wedge() {
    // A pure wedge: every beat sees zero events. The first zero always
    // emits (transition into idle) and then every N-th zero emits — but
    // the thread NEVER retires, because retirement is only allowed after
    // a run of healthy beats.
    let mut state = HeartbeatState::default();
    let first = state.observe(0);
    assert!(first.emit, "first zero-event beat must emit (wedge signal)");
    assert!(!first.retire);
    // Feed enough zeros to well exceed HEARTBEAT_HEALTHY_QUOTA. None of
    // them may retire the thread.
    for _ in 0..(HEARTBEAT_HEALTHY_QUOTA + 100) {
        let a = state.observe(0);
        assert!(!a.retire, "zero-event beats must not retire the heartbeat");
    }
}

#[test]
fn heartbeat_state_retires_after_a_full_healthy_run() {
    // Simulate a genuinely healthy session: every beat carries events.
    // The thread must retire exactly at HEARTBEAT_HEALTHY_QUOTA.
    let mut state = HeartbeatState::default();
    for i in 1..HEARTBEAT_HEALTHY_QUOTA {
        let a = state.observe(1);
        assert!(a.emit, "healthy beats always emit");
        assert!(!a.retire, "must not retire before quota reached (i={i})");
    }
    let final_beat = state.observe(1);
    assert!(final_beat.emit);
    assert!(
        final_beat.retire,
        "must retire on the HEARTBEAT_HEALTHY_QUOTA-th healthy beat"
    );
}

#[test]
fn heartbeat_state_zero_beat_resets_the_healthy_run() {
    // A wedge that appears late (after some healthy beats) must reset the
    // healthy counter, so the thread does NOT retire during the possibly
    // wedged window. Otherwise the diagnostic signal we care most about
    // would disappear right when it matters.
    let mut state = HeartbeatState::default();
    for _ in 0..(HEARTBEAT_HEALTHY_QUOTA - 1) {
        let a = state.observe(1);
        assert!(!a.retire);
    }
    // One zero-event beat — the healthy counter resets. Even if the very
    // next beat is healthy, we should not retire (we need another full
    // healthy_quota run).
    let z = state.observe(0);
    assert!(z.emit, "the zero-event transition must emit");
    assert!(!z.retire);
    let post = state.observe(1);
    assert!(post.emit);
    assert!(
        !post.retire,
        "after a zero-event reset, retirement must wait for another full healthy run"
    );
}

#[test]
fn heartbeat_state_coalesces_idle_beats() {
    // On a genuinely idle session (nobody at the keyboard), the emit
    // cadence must fall to one line every HEARTBEAT_IDLE_EMIT_EVERY
    // beats — otherwise the tee file grows 12/min forever.
    //
    // Emit schedule with HEARTBEAT_IDLE_EMIT_EVERY = N and beats counted
    // by consecutive idle_run:
    //
    //   idle_run = 1               emit (active -> idle transition)
    //   idle_run = 2..N-1          coalesced (no emit)
    //   idle_run = N, 2N, 3N, ...  emit (every N-th)
    //
    // So for N = 10 the emitting beats are 1, 10, 20, ...
    let n = HEARTBEAT_IDLE_EMIT_EVERY;
    assert!(
        n >= 2,
        "coalesce test only meaningful for HEARTBEAT_IDLE_EMIT_EVERY >= 2"
    );
    let mut state = HeartbeatState::default();
    // idle_run = 1: emit (transition into idle).
    let first = state.observe(0);
    assert!(first.emit, "first zero-event beat must emit");
    // idle_run = 2..N-1: coalesced.
    for idle_run in 2..n {
        let a = state.observe(0);
        assert!(
            !a.emit,
            "idle beat {idle_run} within the coalesce window (2..{n}) must not emit"
        );
    }
    // idle_run = N: emit (every N-th idle beat).
    let nth = state.observe(0);
    assert!(nth.emit, "the N-th consecutive idle beat must emit");
    // idle_run = N+1..2N-1: coalesced again (N-1 beats).
    for idle_run in (n + 1)..(2 * n) {
        let a = state.observe(0);
        assert!(
            !a.emit,
            "idle beat {idle_run} within the second coalesce window must not emit"
        );
    }
    // idle_run = 2N: emit.
    let two_n = state.observe(0);
    assert!(two_n.emit, "the 2N-th consecutive idle beat must emit");
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
