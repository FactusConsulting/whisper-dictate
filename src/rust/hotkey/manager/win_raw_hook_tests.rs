//! Companion tests for [`crate::hotkey::manager::win_raw_hook`].
//!
//! The production module is `#![cfg(windows)]`; these tests inherit the
//! same gate so a non-Windows CI job compiles the file to nothing (no
//! test failures because there are simply no tests to run). Every
//! non-hook test exercises pure helpers so no `SetWindowsHookExW` call
//! actually happens — the install test is skipped when the diag level
//! is not `deep` (the install path is a no-op in that case, and CI
//! environments without a message-pump-capable thread would otherwise
//! flake).

#![cfg(all(test, windows))]

use crate::hotkey::manager::win_raw_hook::{
    should_log_raw_hook_event, wm_message_name, RAW_HOOK_INITIAL_TRACE, RAW_HOOK_TRACE_EVERY,
};

// ---------------------------------------------------------------------
// wm_message_name — stable mapping from LL-hook wparam to a short
// grep-friendly name for the trace-line `wm=` field. A regression here
// (say, mis-mapping WM_SYSKEYDOWN as WM_KEYDOWN) would silently make
// the Windows F9 investigation report the wrong message type — and
// SYSKEYDOWN vs KEYDOWN is the interesting distinction for Alt+Fn
// chords, which is exactly the class of key we're investigating.
// ---------------------------------------------------------------------

#[test]
fn wm_message_name_maps_the_four_ll_hook_message_types() {
    assert_eq!(wm_message_name(0x0100), "WM_KEYDOWN");
    assert_eq!(wm_message_name(0x0101), "WM_KEYUP");
    assert_eq!(wm_message_name(0x0104), "WM_SYSKEYDOWN");
    assert_eq!(wm_message_name(0x0105), "WM_SYSKEYUP");
}

#[test]
fn wm_message_name_reports_unknown_values_verbatim() {
    // A future Windows change that starts delivering a new wparam
    // type must be visible in the log — not silently classified as
    // WM_KEYDOWN. The `WM_UNKNOWN(0x…)` prefix keeps the trace line
    // grep-friendly.
    let name = wm_message_name(0xdead);
    assert!(
        name.starts_with("WM_UNKNOWN(0x"),
        "unknown wparam must be reported as WM_UNKNOWN(...), got {name}"
    );
    assert!(
        name.contains("dead"),
        "unknown wparam value must appear verbatim in the name, got {name}"
    );
}

// ---------------------------------------------------------------------
// should_log_raw_hook_event — rate limiter for the raw-hook trace.
// The LL hook fires on EVERY desktop-wide keydown/keyup, so a
// regression that flooded the log (say, forgot the `is_multiple_of`
// check and logged everything after the initial burst) would blow up
// the tee file and slow the pump thread enough to skew the very
// timing the diagnostic is trying to measure.
// ---------------------------------------------------------------------

#[test]
fn should_log_raw_hook_event_prints_the_initial_burst() {
    // First RAW_HOOK_INITIAL_TRACE events always log so a user
    // pressing F9 once in the first second after install is
    // guaranteed to leave a trace line.
    for n in 1..=RAW_HOOK_INITIAL_TRACE {
        assert!(
            should_log_raw_hook_event(n),
            "should_log_raw_hook_event({n}) must be true — event is inside \
             the initial burst window (1..={RAW_HOOK_INITIAL_TRACE})"
        );
    }
}

#[test]
fn should_log_raw_hook_event_skips_between_burst_and_first_multiple() {
    // After the burst but before the first multiple-of-N, every
    // event is suppressed. If this ever regresses to `true` for one
    // of these indices the log would be flooded during typing.
    let start = RAW_HOOK_INITIAL_TRACE + 1;
    let stop = (RAW_HOOK_INITIAL_TRACE / RAW_HOOK_TRACE_EVERY + 1) * RAW_HOOK_TRACE_EVERY;
    for n in start..stop {
        assert!(
            !should_log_raw_hook_event(n),
            "should_log_raw_hook_event({n}) must be false — event is in the \
             suppressed range ({start}..{stop})"
        );
    }
}

#[test]
fn should_log_raw_hook_event_prints_multiples_of_trace_every() {
    // After the burst, only multiples of RAW_HOOK_TRACE_EVERY log —
    // proves forward progress in long sessions without flooding.
    for k in 5..15 {
        let n = RAW_HOOK_TRACE_EVERY * k;
        // Skip the initial burst window if it happens to cover this k.
        if n <= RAW_HOOK_INITIAL_TRACE {
            continue;
        }
        assert!(
            should_log_raw_hook_event(n),
            "should_log_raw_hook_event({n}) must be true — multiples of \
             RAW_HOOK_TRACE_EVERY log so long sessions show forward progress"
        );
        // A value one before the multiple must NOT log (sampling, not
        // summing).
        assert!(
            !should_log_raw_hook_event(n - 1),
            "should_log_raw_hook_event({}) must be false — only exact multiples \
             of RAW_HOOK_TRACE_EVERY satisfy the every-Nth rule",
            n - 1
        );
    }
}

// ---------------------------------------------------------------------
// install() gate: at Info/default level, install() is a no-op — the
// diagnostic hook must NOT install unless the operator opts in via
// `VOICEPI_LOG=trace`. Regressing that would install an LL hook on
// every user's box even when they're not investigating anything.
// ---------------------------------------------------------------------

#[test]
fn install_is_a_noop_at_default_info_level() {
    // Save + reset the level so the test doesn't leak the operator's
    // shell-set `VOICEPI_LOG=trace` (if any) into subsequent tests.
    // Ordering: reset_level_for_tests puts the atomic back to Info,
    // which is the release default — install() should refuse.
    let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    crate::diag::reset_level_for_tests();

    let installed = crate::hotkey::manager::win_raw_hook::install();
    assert!(
        !installed,
        "install() must be a no-op at Info level so the LL diagnostic hook \
         only spawns when the operator sets VOICEPI_LOG=trace"
    );
    // The latch stays clear so a later `trace` opt-in can still
    // install.
    assert!(
        !crate::hotkey::manager::win_raw_hook::is_installed(),
        "the INSTALLED latch must remain clear when install() was a no-op"
    );
}

#[test]
fn should_log_raw_hook_event_zero_is_never_a_valid_index() {
    // Counter is 1-indexed (fetch_add returns previous + 1). A stray
    // 0 would make the log file depend on unrelated startup ordering;
    // suppress rather than log.
    assert!(
        !should_log_raw_hook_event(0),
        "should_log_raw_hook_event(0) must be false — the counter is 1-indexed \
         and 0 indicates a caller bug"
    );
}
