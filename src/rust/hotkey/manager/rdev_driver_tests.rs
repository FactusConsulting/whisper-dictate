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

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::hotkey::inject_guard::InjectionGuard;
use crate::hotkey::manager::rdev_driver::{
    is_rdev_supported_name, listener_readiness_handshake, raw_from_rdev,
    redact_event_type_for_debug, redact_raw_event_name, should_log_raw_event, spawn,
    spawn_heartbeat_thread, spawn_heartbeat_thread_with_config,
    spawn_with_raw_tap_capturing_heartbeat_for_tests, HeartbeatState, ListenerAbort,
    ListenerSignal, ListenerStart, NoopRawTap, SpawnError, HEARTBEAT_HEALTHY_QUOTA,
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
        Err(SpawnError::WriterStartup(msg)) => {
            eprintln!(
                "skipping register_and_unregister_roundtrip: diag async \
                 writer thread refused to spawn ({msg})"
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
        Err(SpawnError::WriterStartup(msg)) => {
            // Codex P2 #675 PRRT_kwDOSfNjQs6UbAip: the diag async
            // writer thread failed to spawn. This is a rare
            // environment-specific outcome (OS refusing thread
            // creation) — treat it as "tell the caller" the same
            // way ListenerHung is treated.
            assert!(
                !msg.is_empty(),
                "WriterStartup error message should not be empty"
            );
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
        Err(SpawnError::ListenerStartup(_))
        | Err(SpawnError::ListenerHung)
        | Err(SpawnError::WriterStartup(_)) => {
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
// Codex P2 #675 PRRT_kwDOSfNjQs6Ub__s — a listener whose readiness
// receiver already timed out MUST abort before installing rdev's
// process-lifetime global hook. `rdev::listen` blocks forever and has
// no stop API, so a listener that runs on after `spawn` returned
// `SpawnError::ListenerHung` is an unreclaimable orphan sitting beside
// whatever fallback (or retry) the caller installs next — both hooks
// then do the callback work and both write diagnostics.
// -----------------------------------------------------------------------

#[test]
fn listener_readiness_handshake_aborts_when_the_waiter_is_gone() {
    // Reproduce the exact post-timeout shape: `spawn`'s
    // `recv_timeout(READY_PROBE_WINDOW)` expired, it returned
    // `SpawnError::ListenerHung`, and `ready_rx` was dropped on the way
    // out. The listener thread reaches its handshake afterwards.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<ListenerSignal>();
    drop(ready_rx);

    let outcome = listener_readiness_handshake(&ready_tx, Ok(()));
    assert_eq!(
        outcome,
        ListenerStart::Abort(ListenerAbort::ReadinessWaiterGone),
        "with the readiness receiver dropped the listener must abort BEFORE \
         rdev::listen - the pre-fix code ignored the failed `Started` send \
         (`let _ = ...`) and installed an un-stoppable global hook anyway \
         (Codex P2 #675 PRRT_kwDOSfNjQs6Ub__s)"
    );
    assert_eq!(
        ListenerAbort::ReadinessWaiterGone.as_str(),
        "readiness receiver was dropped (spawn already timed out)",
        "the abort reason is the only breadcrumb this path leaves in \
         gui-diagnostic.log; keep it stable and specific"
    );
}

#[test]
fn listener_readiness_handshake_proceeds_while_the_waiter_listens() {
    // The healthy case must be unchanged: `Started` lands on the
    // channel and the listener proceeds into rdev::listen. Without this
    // assertion an over-eager abort fix would silently disable the
    // whole rdev backend.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<ListenerSignal>();

    let outcome = listener_readiness_handshake(&ready_tx, Ok(()));
    assert_eq!(
        outcome,
        ListenerStart::Proceed,
        "a live readiness waiter must let the listener install the hook"
    );
    assert!(
        matches!(ready_rx.try_recv(), Ok(ListenerSignal::Started)),
        "the handshake must announce Started so spawn can tell 'thread \
         never scheduled' apart from 'rdev is blocking healthily'"
    );
}

#[test]
fn listener_readiness_handshake_reports_writer_failure_instead_of_started() {
    // Writer priming failure keeps its pre-existing precedence: report
    // WriterFailed (→ SpawnError::WriterStartup) and never announce
    // readiness. Codex P2 #675 PRRT_kwDOSfNjQs6UbAip, re-pinned here so
    // the abort refactor cannot reorder the two branches.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<ListenerSignal>();

    let outcome = listener_readiness_handshake(&ready_tx, Err("thread refused".to_owned()));
    assert_eq!(
        outcome,
        ListenerStart::Abort(ListenerAbort::WriterStartup),
        "a writer-priming failure must abort the listener"
    );
    match ready_rx.try_recv() {
        Ok(ListenerSignal::WriterFailed(msg)) => assert!(
            msg.contains("thread refused"),
            "the writer error text must reach the caller verbatim, got {msg:?}"
        ),
        other => panic!(
            "expected WriterFailed as the FIRST signal, got a different variant: {}",
            match other {
                Ok(ListenerSignal::Started) => "Started",
                Ok(ListenerSignal::Failed(_)) => "Failed",
                Ok(ListenerSignal::WriterFailed(_)) => "WriterFailed",
                Err(_) => "nothing",
            }
        ),
    }
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

// -----------------------------------------------------------------------
// Codex P2 #657 r3663766095 — the heartbeat thread MUST actually exit
// when `stop` is set. The prior `spawn_startup_failure_stops_heartbeat_thread`
// test only asserted that `spawn` returned promptly, which was already
// true before the heartbeat_stop lifecycle fix — removing every stop
// store would leave the orphan heartbeat running while the test still
// passed. This regression pins the exit through the JoinHandle now
// returned by `spawn_heartbeat_thread`.
//
// Failure mode against the un-fixed code (the old fn ignored the
// spawn result with `let _ = ...`, so it had no way to expose a
// JoinHandle at all): the test would not compile — the import of
// `spawn_heartbeat_thread` (which used to be a private `fn`
// returning `()`) would surface it, and the `.join()` call below
// would type-check only against the new `Result<JoinHandle<()>>`
// return.
// -----------------------------------------------------------------------

#[test]
fn spawn_heartbeat_thread_exits_when_stop_is_signalled() {
    use std::time::{Duration, Instant};

    let total = Arc::new(AtomicU64::new(0));
    let since = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let handle = spawn_heartbeat_thread(Arc::clone(&total), Arc::clone(&since), Arc::clone(&stop))
        .expect("heartbeat thread must spawn on any test host");

    // Signal stop and give the sliced sleep in the heartbeat loop
    // (250 ms per slice) a bounded number of chances to observe it.
    // The heartbeat re-checks `stop` every 250 ms; a healthy exit
    // happens on the FIRST re-check after `store`, so 2 seconds is
    // ~8 slices of slack — comfortably above the sleep granularity
    // while still keeping the test suite fast.
    stop.store(true, Ordering::Relaxed);
    let join_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < join_deadline && !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        handle.is_finished(),
        "heartbeat thread must observe `stop` and exit within the deadline; \
         the pre-fix code never returned the JoinHandle so this invariant \
         could not be pinned — Codex P2 #657 r3663766095",
    );
    // Join to reap the thread + surface any panic that fired inside
    // the heartbeat body. `.join()` on a finished handle is
    // effectively free.
    handle.join().expect("heartbeat thread panicked");
}

#[test]
fn spawn_heartbeat_thread_reaches_in_loop_retirement_via_config_shim() {
    // Codex P2 #673 thread PRRT_kwDOSfNjQs6UaDch: the earlier
    // `spawn_heartbeat_thread_exits_on_retirement_even_without_external_stop`
    // test signalled `stop` from OUTSIDE the loop and therefore did not
    // exercise the retirement branch at all. Deleting the in-loop
    // `stop.store(true)` + `return` would have left it green while the
    // real self-retire path rotted.
    //
    // The parametrised `spawn_heartbeat_thread_with_config` shim lets
    // us drive the retirement branch on a millisecond timescale by
    // shrinking the interval and healthy-run quota. A background
    // "poker" thread primes `since` before each beat so `observe`
    // sees since>0, increments healthy_run, and eventually flips
    // `stop` from INSIDE the loop. The post-exit `stop.load()`
    // assertion is the key: only the in-loop `stop.store(true)` in
    // the retirement branch could have set it (we never set it from
    // this test).
    use std::time::{Duration, Instant};
    let total = Arc::new(AtomicU64::new(0));
    let since = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let handle = spawn_heartbeat_thread_with_config(
        Arc::clone(&total),
        Arc::clone(&since),
        Arc::clone(&stop),
        Duration::from_millis(5),
        3, // tiny quota
    )
    .expect("heartbeat thread must spawn");
    // Prime `since` before each beat so `observe` sees since>0 and
    // increments healthy_run. Poker thread pumps since=1 every 2ms.
    let since_poker = Arc::clone(&since);
    let poker = std::thread::spawn(move || {
        for _ in 0..500 {
            since_poker.store(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    // Wait for the in-loop self-retire to flip stop and exit.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        handle.is_finished(),
        "in-loop retirement branch must fire and exit within budget"
    );
    assert!(
        stop.load(Ordering::Relaxed),
        "the retirement branch's `stop.store(true)` MUST have been observed — \
         proves the internal path fired; a regression that removes that store \
         would leave `stop` false here even though the thread eventually exits \
         via the outer while-guard (Codex P2 #673 PRRT_kwDOSfNjQs6UaDch)"
    );
    handle.join().expect("heartbeat panicked");
    poker.join().ok();
}

// -----------------------------------------------------------------------
// Codex P2 #673 thread PRRT_kwDOSfNjQs6UaDcc — the actual spawn wiring
// must stop the heartbeat on the startup-failure early-return paths.
//
// The `spawn_heartbeat_thread_exits_when_stop_is_signalled` test above
// flips an INDEPENDENT stop atomic, which only proves the heartbeat
// loop honours a stop signal — it does NOT prove that
// `spawn_with_raw_tap`'s error branches actually set the atomic.
// Deleting every `heartbeat_stop.store(true, ...)` from those branches
// would still leave that test green while orphan heartbeats
// accumulated on retry. This test drives the real wiring via the
// crate-only `spawn_with_raw_tap_capturing_heartbeat_for_tests`
// shim — on any host where rdev::listen returns Err quickly (headless
// CI), the shim's returned heartbeat JoinHandle must observe the
// spawn-wiring's `heartbeat_stop.store(true, ...)` and exit.
// -----------------------------------------------------------------------

#[test]
fn spawn_startup_failure_actually_stops_the_heartbeat_via_wiring() {
    use std::time::{Duration, Instant};
    let guard = Arc::new(InjectionGuard::new());
    let (result, heartbeat) =
        spawn_with_raw_tap_capturing_heartbeat_for_tests(guard, |_out| {}, NoopRawTap);
    let heartbeat = heartbeat.expect(
        "heartbeat thread must spawn on any reasonable test host — OOM on \
         thread::Builder::spawn is the only failure mode",
    );
    match result {
        Ok((handle, _thread)) => {
            // Host actually has a working display / accessibility perms —
            // the wiring's error branches don't fire here, so this
            // regression is only meaningful in a headless environment.
            // Clean up and skip the assertion; the paired invariant is
            // covered by the failing-startup branch on CI.
            handle.shutdown();
            eprintln!(
                "skipping spawn_startup_failure_actually_stops_the_heartbeat_via_wiring: \
                 rdev listener started successfully on this host, so the wiring's \
                 error-branch `heartbeat_stop.store(true)` was not exercised"
            );
            return;
        }
        Err(SpawnError::ListenerStartup(_))
        | Err(SpawnError::ListenerHung)
        | Err(SpawnError::WriterStartup(_)) => {
            // Fall through — we expect the heartbeat to exit because
            // one of the wiring's error branches stored heartbeat_stop.
            // `WriterStartup` (Codex P2 #675 PRRT_kwDOSfNjQs6UbAip) is
            // one of those branches too: the rebase onto #673's
            // tuple-returning `spawn_with_raw_tap_inner` made it share
            // the same `(Err(..), heartbeat_handle)` early-return shape.
        }
    }
    // Wait up to 3 s for the heartbeat's slice-sleep to observe the
    // stop atomic set by the spawn wiring's error branch. Failure
    // mode against a regression that removes the wiring stores: the
    // heartbeat runs forever, is_finished() stays false, we hit the
    // deadline, and the assert below trips.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !heartbeat.is_finished() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        heartbeat.is_finished(),
        "spawn's startup-failure early-return branches MUST store to \
         heartbeat_stop so the heartbeat thread exits — otherwise a caller \
         that retries after a listener-startup failure accumulates orphan \
         heartbeat threads (Codex P2 #673 PRRT_kwDOSfNjQs6UaDcc)"
    );
    heartbeat.join().expect("heartbeat thread panicked");
}
