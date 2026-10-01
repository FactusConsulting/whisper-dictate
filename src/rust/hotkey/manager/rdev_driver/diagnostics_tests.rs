//! diagnostics regression tests for the rdev driver.

use super::*;

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
// #646 r3661145597 — redact non-PTT global keystrokes.
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
// #657 r3663766123 — redact pre-filter `[rdev/callback] raw=` trace.
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
// #646 r3661145589 — off-callback trace writer.
//
// The LL-hook callback on Windows has a strict per-call time budget (a
// few milliseconds) enforced by the OS; if it ever runs over, Windows
// silently unhooks the callback — the exact PTT-wedge this
// instrumentation was written to diagnose. So the callback MUST NOT
// perform synchronous file I/O; every trace line goes through a bounded,
// non-blocking mpsc queue that a dedicated writer thread drains into
// `crate::diag::write_line`. These tests exercise the queue's non-
// blocking semantics without spawning the OS listener.
// -----------------------------------------------------------------------

#[test]
fn callback_trace_queue_capacity_is_bounded() {
    // The capacity is a compile-time constant tuned for realistic
    // rate-limited bursts. A regression that made it unbounded (e.g. by
    // swapping `sync_channel` for `channel`) would remove the drop
    // semantics and let a slow AppData volume back up the callback via
    // memory pressure instead of an I/O stall.
    //
    // Copy through a runtime binding so clippy's `assertions_on_constants`
    // lint doesn't flag the compile-time comparison. The tuning check
    // is a real regression signal — an accidental multiplication by
    // 1000 would still trip the ceiling.
    let cap = CALLBACK_TRACE_QUEUE_CAPACITY;
    assert!(
        cap > 0,
        "queue must have a non-zero capacity or the writer never starts"
    );
    assert!(
        cap < 100_000,
        "queue capacity {cap} looks unbounded — regression?"
    );
}

#[test]
fn enqueue_callback_trace_does_not_block_when_writer_is_absent() {
    // Before `ensure_callback_trace_writer` runs, the OnceLock is empty
    // and `enqueue_callback_trace` must return promptly (early-out on
    // the `.get()` miss) rather than block on any lock. Regression: a
    // future refactor that lazy-initialised the writer inside the
    // enqueue path could re-introduce blocking on the hot path.
    //
    // We can't guarantee the OnceLock is empty (another test may have
    // populated it), but we CAN bound the enqueue time. On a working
    // implementation this is microseconds; a regression would spin or
    // block for the write duration.
    //
    // Serialised against the diagnostic-log tests: these 1000 enqueues
    // share the process-wide bounded queue and writer thread, so
    // without the lock they can evict a concurrent tracker test's
    // `[chord]` line or stall its `flush_async_for_tests` wait.
    // #668  3666690064.
    let _diag_lock = diag_test_lock();
    let start = std::time::Instant::now();
    for i in 0..1000 {
        enqueue_callback_trace(format!("trace-nop-{i}"));
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "1000 enqueues took {elapsed:?}; the callback path must be effectively free"
    );
    // Drain before releasing the lock. Holding it only across the
    // enqueues is not enough: the writer thread keeps draining after
    // this test returns, so the next diagnostic-log test could still
    // acquire the lock while our backlog is in flight and blow its
    // `flush_async_for_tests` budget. #668  3666690064.
    crate::diag::flush_async_for_tests();
}

#[test]
fn enqueue_callback_trace_after_writer_install_still_returns_immediately() {
    // With the writer thread running, try_send returns immediately on
    // success AND drops immediately on Full. Neither path blocks the
    // caller — that's the whole point of the queue on the LL-hook thread.
    // Pump enough lines to certainly exceed the queue capacity so we
    // exercise both branches; the enqueue call is still bounded time.
    //
    // This test DELIBERATELY overfills the shared bounded queue, so it
    // is the more dangerous of the two floods to leave unserialised —
    // hold the crate-wide diagnostic lock across it. #668
    //  3666690064.
    let _diag_lock = diag_test_lock();
    ensure_callback_trace_writer_for_tests();
    let start = std::time::Instant::now();
    let burst = CALLBACK_TRACE_QUEUE_CAPACITY * 4;
    for i in 0..burst {
        enqueue_callback_trace(format!("trace-flood-{i}"));
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "flooding {burst} enqueues took {elapsed:?}; try_send must never block \
         on a full queue or the LL-hook callback wedges"
    );
    // Drain the backlog before releasing the lock — see the sibling
    // flood test for why holding it across the enqueues alone leaves a
    // window open. #668  3666690064.
    crate::diag::flush_async_for_tests();
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
