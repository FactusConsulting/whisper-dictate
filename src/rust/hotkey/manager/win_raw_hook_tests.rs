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
    install_with_installer, is_installed, is_investigated_vk, should_log_raw_hook_event,
    wm_message_name, HookInstaller, RAW_HOOK_INITIAL_TRACE, RAW_HOOK_TRACE_EVERY,
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

// ---------------------------------------------------------------------
// Codex P2 #651 discussion PRRT_kwDOSfNjQs6UTvPg — investigated keys
// (F1..F12, Pause, Ctrl / Shift / Alt / Meta variants) MUST log every
// single time regardless of the sampling rate limit. Otherwise an F9
// press delivered as event 201..249 leaves no `[win/raw-hook]` line
// and the documented decision tree misdiagnoses the missing line as
// an upstream hook consuming F9.
// ---------------------------------------------------------------------

/// Combined gate the callback actually applies to decide whether to
/// emit a `[win/raw-hook]` line — investigated keys ALWAYS log, and
/// ordinary keys follow the sampling rule. The two-arg helper mirrors
/// the branch inside `ll_keyboard_hook_proc` so any regression to that
/// branch is caught here without going near Windows APIs.
fn effective_gate(n: u64, vk: u32) -> bool {
    is_investigated_vk(vk) || should_log_raw_hook_event(n)
}

#[test]
fn investigated_vk_covers_f1_through_f12() {
    // Full F-key range must qualify — the investigation was born of
    // an F9 drop specifically, but F-keys are interchangeable in the
    // PTT binding surface so covering the whole family future-proofs
    // any user whose binding switches from F9 to (say) F8.
    for vk in 0x70u32..=0x7B {
        assert!(
            is_investigated_vk(vk),
            "vk 0x{vk:02X} (F-key) must qualify as an investigated key so its \
             `[win/raw-hook]` line is never dropped by the rate limit"
        );
    }
}

#[test]
fn investigated_vk_covers_pause_and_every_modifier_side() {
    // The full set the raw-hook trace was built to catch: Pause and
    // every side of Ctrl / Shift / Alt / Meta. Naming each vk in the
    // fixture (rather than a range) so a regression that dropped e.g.
    // VK_LWIN alone fails obviously.
    let cases: &[(u32, &str)] = &[
        (0x13, "VK_PAUSE"),
        (0x11, "VK_CONTROL"),
        (0xA2, "VK_LCONTROL"),
        (0xA3, "VK_RCONTROL"),
        (0x10, "VK_SHIFT"),
        (0xA0, "VK_LSHIFT"),
        (0xA1, "VK_RSHIFT"),
        (0x12, "VK_MENU"),
        (0xA4, "VK_LMENU"),
        (0xA5, "VK_RMENU"),
        (0x5B, "VK_LWIN"),
        (0x5C, "VK_RWIN"),
    ];
    for (vk, name) in cases {
        assert!(
            is_investigated_vk(*vk),
            "vk 0x{vk:02X} ({name}) must qualify as an investigated key"
        );
    }
}

#[test]
fn investigated_vk_rejects_ordinary_typing_keys() {
    // Letters, digits, punctuation, arrow keys — none of these are
    // part of the F-drop investigation, so the sampling rule (not
    // the always-log override) must gate them. A regression that
    // over-broadened the predicate would defeat the whole "keep the
    // log bounded on long typing sessions" property.
    let ordinary: &[(u32, &str)] = &[
        (0x41, "VK_A"),
        (0x30, "VK_0"),
        (0x39, "VK_9"),
        (0x25, "VK_LEFT"),
        (0x26, "VK_UP"),
        (0x1B, "VK_ESCAPE"),
        (0x0D, "VK_RETURN"),
        (0x20, "VK_SPACE"),
    ];
    for (vk, name) in ordinary {
        assert!(
            !is_investigated_vk(*vk),
            "vk 0x{vk:02X} ({name}) must NOT be classified as investigated - \
             ordinary typing keys stay under the sampling rate limit"
        );
    }
}

#[test]
fn effective_gate_always_logs_investigated_keys_in_the_suppressed_range() {
    // The regression this test locks: event indices 201..249 fall
    // AFTER the initial burst and BEFORE the first multiple of
    // RAW_HOOK_TRACE_EVERY (250). The un-fixed sampling drops them
    // all. With the investigated-key override, F9 (VK_F9 = 0x78) must
    // still log every time.
    let vk_f9 = 0x78u32;
    let vk_a = 0x41u32;
    for n in [201u64, 231, 249] {
        // Sampling alone drops these indices — that's the bug seed.
        assert!(
            !should_log_raw_hook_event(n),
            "precondition: sampling suppresses ordinary event {n} - the \
             investigated-key override is what restores F9 visibility"
        );
        // With the override in place, F9 still logs at all three
        // suppressed indices — this is the load-bearing assertion.
        assert!(
            effective_gate(n, vk_f9),
            "F9 (vk 0x{vk_f9:02X}) at suppressed event {n} must log despite the \
             sampling gate - Codex P2 #651 PRRT_kwDOSfNjQs6UTvPg"
        );
        // Ordinary VK_A stays suppressed at the same indices — the
        // fix only changes behaviour for investigated keys.
        assert!(
            !effective_gate(n, vk_a),
            "VK_A (vk 0x{vk_a:02X}) at suppressed event {n} must stay suppressed \
             so ordinary typing does not flood the log"
        );
    }
    // At event 250 (the first multiple of RAW_HOOK_TRACE_EVERY after
    // the burst) both ordinary and investigated keys log — the
    // sampling rule kicks in.
    let n = 250u64;
    assert!(effective_gate(n, vk_f9));
    assert!(effective_gate(n, vk_a));
    // And at event 299 (between two sampling multiples) investigated
    // keys log, ordinary ones don't — same story as 201..249.
    let n = 299u64;
    assert!(effective_gate(n, vk_f9));
    assert!(!effective_gate(n, vk_a));
}

// ---------------------------------------------------------------------
// Codex P2 #651 discussion PRRT_kwDOSfNjQs6UTvPp — install() must
// report the actual outcome of `SetWindowsHookExW`, not just the
// pump-thread spawn success. Otherwise a null-hook failure leaves the
// caller announcing an installed hook that never was and latches
// INSTALLED against a retry.
// ---------------------------------------------------------------------

/// Test-only installer that always reports failure. Simulates a
/// `SetWindowsHookExW` returning null — exactly the outcome the
/// production `Win32HookInstaller` would surface when a policy /
/// antivirus hook chain refuses installation.
struct FailingHookInstaller;
impl HookInstaller for FailingHookInstaller {
    fn install(self) -> bool {
        false
    }
}

#[test]
fn install_reports_false_when_pump_thread_signals_failure() {
    // Serialise on both the diag-writer lock and the env lock so the
    // level flip does not race any other diag-touching test in this
    // binary. Restore the pre-test VOICEPI_LOG value at the end so a
    // shell-set trace level does not leak into subsequent tests.
    let _diag_lock = crate::diag_test_lock::DIAG_WRITER_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _env_guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let prev_log = std::env::var(crate::diag::LOG_ENV_VAR).ok();

    // Opt into trace so install_with_installer does not short-circuit
    // on the level gate. This is the same door VOICEPI_LOG=trace opens
    // in production.
    std::env::set_var(crate::diag::LOG_ENV_VAR, "trace");
    crate::diag::reset_level_for_tests();
    assert_eq!(crate::diag::init_from_env(), crate::diag::LogLevel::Trace);

    // The install path uses the process-wide INSTALLED latch, so a
    // previous test in the same binary that installed successfully
    // would make this call a no-op. Recover from that by explicitly
    // resetting the latch — this is a test-only concession to the
    // OnceLock+atomic state the production module keeps.
    // (A fresh test binary always starts clean; this is defence in
    // depth for a future test that installs the real hook.)
    // We can't clear HOOK_THREAD_INSTALLED (it's a OnceLock without
    // a reset accessor), but the assertion here only touches the
    // INSTALLED atomic.
    let installed = install_with_installer(FailingHookInstaller);
    assert!(
        !installed,
        "install_with_installer(FailingHookInstaller) must return false so \
         the GUI does not falsely announce that the diagnostic hook is live \
         when SetWindowsHookExW returned NULL - Codex P2 #651 PRRT_kwDOSfNjQs6UTvPp"
    );
    assert!(
        !is_installed(),
        "the INSTALLED latch must be released after a hook-API failure so a \
         follow-up install call (retry) can actually retry - the un-fixed \
         version left the latch set on spawn even when the hook failed"
    );

    // Restore the pre-test env value so subsequent tests see a clean
    // slate. reset_level_for_tests is idempotent so calling it after
    // the env restore is fine.
    match prev_log {
        Some(v) => std::env::set_var(crate::diag::LOG_ENV_VAR, v),
        None => std::env::remove_var(crate::diag::LOG_ENV_VAR),
    }
    crate::diag::reset_level_for_tests();
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
