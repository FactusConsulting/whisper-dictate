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

use std::sync::mpsc;

use crate::hotkey::manager::win_raw_hook::{
    format_raw_hook_trace_line, install_with_installer, is_installed, is_investigated_vk,
    reset_installed_for_tests, run_pump_startup, should_log_raw_hook_event, wm_message_name,
    HookInstaller, RAW_HOOK_INITIAL_TRACE, RAW_HOOK_TRACE_EVERY,
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
    // would make this call a no-op. Reset the latch so this test
    // starts from a known clean state.
    reset_installed_for_tests();
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

// ---------------------------------------------------------------------
// Codex P2 #675 PRRT_kwDOSfNjQs6UbAii — the sampled `[win/raw-hook]`
// trace line MUST go via the async diagnostic writer, not the
// synchronous `crate::diag::log!` path. Previously the LL-hook
// callback synchronously flushed the AppData tee file on every
// investigated / sampled event, which reintroduces the ~300 ms
// callback timeout the parallel hook was built to diagnose.
//
// The pure formatter test pins the exact line the callback enqueues
// so a regression to a synchronous write would be catchable via
// grep-shape assertions AND the formatter's stability contract.
// ---------------------------------------------------------------------

#[test]
fn format_raw_hook_trace_line_produces_grep_friendly_shape() {
    let line = format_raw_hook_trace_line(
        231,    // n — an event in the suppressed range
        0x0104, // wparam — WM_SYSKEYDOWN (Alt+F9 case)
        0x78,   // vk — VK_F9, one of the investigated keys
        0x43,   // scan
        0x20,   // flags
        false,  // injected
        false,  // extended
        true,   // investigated
    );
    assert!(
        line.starts_with("[win/raw-hook] "),
        "raw-hook line must begin with `[win/raw-hook] ` so support runbook \
         greps keep working; got: {line:?}"
    );
    assert!(line.contains("#231"));
    assert!(line.contains("wm=WM_SYSKEYDOWN"));
    assert!(line.contains("vk=0x78"));
    assert!(line.contains("scan=0x43"));
    assert!(line.contains("investigated=true"));
    assert!(line.contains("injected=false"));
    assert!(line.contains("extended=false"));
}

#[test]
fn format_raw_hook_trace_line_reports_injected_and_extended_bits() {
    // The whole point of the injected/extended fields is to catch a
    // stray SendInput from within our own process (injected=true is
    // the regression-in-injection-guard signal). Pin the exact
    // rendering so a boolean-flip regression fails here.
    let line = format_raw_hook_trace_line(1, 0x0100, 0x41, 0x1E, 0x11, true, true, false);
    assert!(
        line.contains("injected=true"),
        "injected bit must render as `injected=true`; got: {line:?}"
    );
    assert!(
        line.contains("extended=true"),
        "extended bit must render as `extended=true`; got: {line:?}"
    );
}

// ---------------------------------------------------------------------
// Codex P2 #675 PRRT_kwDOSfNjQs6UbAiO — a `SetWindowsHookExW` call
// that succeeds AFTER `INSTALL_READY_TIMEOUT` must NOT release the
// INSTALLED latch. Releasing the latch on timeout reopens a window
// where the pump thread eventually installs a live hook while a
// retry-happy caller sees `installed=false`, calls `install()`
// again, and stacks a second process-lifetime hook.
// ---------------------------------------------------------------------

/// Test-only installer that sleeps past `INSTALL_READY_TIMEOUT` then
/// reports success. Simulates a delayed `SetWindowsHookExW` — the
/// exact "slow host" scenario the timeout window was measured
/// against. The pump thread will therefore signal `Started` well
/// after the sync deadline; the fix must keep the INSTALLED latch
/// held pending so a retry cannot stack a second hook.
struct SlowThenSuccessfulInstaller;
impl HookInstaller for SlowThenSuccessfulInstaller {
    fn install(self) -> bool {
        // Sleep past `INSTALL_READY_TIMEOUT` (500 ms). Add a margin
        // so a slow CI scheduler doesn't accidentally deliver the
        // signal in time.
        std::thread::sleep(std::time::Duration::from_millis(750));
        true
    }
}

#[test]
fn install_keeps_latch_pending_when_pump_thread_is_delayed() {
    let _diag_lock = crate::diag_test_lock::DIAG_WRITER_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _env_guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let prev_log = std::env::var(crate::diag::LOG_ENV_VAR).ok();

    std::env::set_var(crate::diag::LOG_ENV_VAR, "trace");
    crate::diag::reset_level_for_tests();
    assert_eq!(crate::diag::init_from_env(), crate::diag::LogLevel::Trace);
    reset_installed_for_tests();

    let installed = install_with_installer(SlowThenSuccessfulInstaller);
    assert!(
        !installed,
        "install must report false on timeout - the pump thread did not \
         signal within INSTALL_READY_TIMEOUT so the caller cannot claim \
         a confirmed install"
    );
    // Load-bearing: the latch STAYS SET. The pre-fix code released
    // it on timeout, so a retry would install a SECOND live hook once
    // the delayed SetWindowsHookExW eventually returned.
    // Codex P2 #675 PRRT_kwDOSfNjQs6UbAiO.
    assert!(
        is_installed(),
        "INSTALLED latch must stay set on install timeout so a retry-happy \
         caller cannot stack a second live pump thread once the delayed \
         SetWindowsHookExW succeeds - Codex P2 #675 PRRT_kwDOSfNjQs6UbAiO"
    );

    // Give the slow installer time to actually finish so the pump
    // thread is not still running when the next test begins.
    std::thread::sleep(std::time::Duration::from_millis(500));
    reset_installed_for_tests();
    match prev_log {
        Some(v) => std::env::set_var(crate::diag::LOG_ENV_VAR, v),
        None => std::env::remove_var(crate::diag::LOG_ENV_VAR),
    }
    crate::diag::reset_level_for_tests();
}

// ---------------------------------------------------------------------
// Codex P2 #675 PRRT_kwDOSfNjQs6UbAim / PRRT_kwDOSfNjQs6UbAin — the
// pump thread's startup ORDER. Both findings are invisible in a
// straight-line read of the thread body, so they are pinned here at
// the `run_pump_startup` seam:
//
//   1. prime the async diagnostic writer BEFORE installing / reporting
//      the hook (otherwise the LL-hook callback does the OnceLock init
//      and thread spawn itself, on the OS hook thread, and a failed
//      spawn silently voids every [win/raw-hook] record while
//      `install()` reports success);
//   2. send the outcome on the readiness channel BEFORE the
//      potentially blocking diagnostic write (otherwise a stalled
//      AppData sink pushes the send past INSTALL_READY_TIMEOUT, the
//      receiver is gone, and the INSTALLED latch — deliberately held
//      on timeout — permanently blocks a retry).
//
// These tests are `#[cfg(all(test, windows))]` along with the rest of
// this file (the production module is `#![cfg(windows)]`), so they run
// on the `rust (windows-2025)` CI leg.
// ---------------------------------------------------------------------

/// Installer that records whether it was invoked and then succeeds.
/// Lets a test prove the writer-priming failure short-circuits BEFORE
/// `SetWindowsHookExW` is ever reached.
struct RecordingHookInstaller(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl HookInstaller for RecordingHookInstaller {
    fn install(self) -> bool {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        true
    }
}

#[test]
fn run_pump_startup_refuses_to_install_when_the_async_writer_failed_to_prime() {
    let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let mut lines: Vec<String> = Vec::new();

    let enter_pump = run_pump_startup(
        RecordingHookInstaller(std::sync::Arc::clone(&called)),
        Err("thread refused".to_owned()),
        &ready_tx,
        &mut |line| lines.push(line.to_owned()),
    );

    assert!(
        !enter_pump,
        "a failed writer prime must abort the pump thread - a live hook whose \
         every record is dropped fakes the exact evidence ('no key events \
         reached the process') this diagnostic exists to disprove"
    );
    assert!(
        !called.load(std::sync::atomic::Ordering::SeqCst),
        "SetWindowsHookExW must NOT be called once the writer prime failed - \
         the writer is primed FIRST so the LL-hook callback never performs the \
         OnceLock init / Builder::spawn itself (Codex P2 #675 \
         PRRT_kwDOSfNjQs6UbAim)"
    );
    match ready_rx.try_recv() {
        Ok(Err(msg)) => assert!(
            msg.contains("thread refused"),
            "the writer error text must reach install_with_installer verbatim, \
             got {msg:?}"
        ),
        other => panic!("expected an Err outcome on the readiness channel, got {other:?}"),
    }
    assert!(
        lines.iter().any(|l| l.starts_with("[win/raw-hook] ")),
        "the refusal must leave a grep-friendly breadcrumb in the tee file"
    );
}

/// Drive `run_pump_startup` and report whether the readiness channel
/// already carried the outcome at the moment the FIRST diagnostic line
/// was emitted. That is the ordering property both P2s ask for, pinned
/// deterministically (no sleeps, no timing assumptions).
fn outcome_visible_at_first_log<I: HookInstaller>(installer: I) -> (bool, Option<bool>) {
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let observed: std::cell::Cell<Option<bool>> = std::cell::Cell::new(None);
    let entered = {
        let mut logger = |_line: &str| {
            if observed.get().is_none() {
                observed.set(Some(ready_rx.try_recv().is_ok()));
            }
        };
        run_pump_startup(installer, Ok(()), &ready_tx, &mut logger)
    };
    (entered, observed.get())
}

#[test]
fn run_pump_startup_signals_hook_failure_before_writing_diagnostics() {
    let (entered, observed) = outcome_visible_at_first_log(FailingHookInstaller);
    assert!(!entered, "a NULL hook must not enter the message pump");
    assert_eq!(
        observed,
        Some(true),
        "the failure outcome must already be on the readiness channel when the \
         first diagnostic line is written - the pre-fix order (log, then send) \
         lets a stalled AppData sink push the send past INSTALL_READY_TIMEOUT, \
         after which the receiver is gone, the send is discarded, and the \
         deliberately-held INSTALLED latch blocks every retry forever \
         (Codex P2 #675 PRRT_kwDOSfNjQs6UbAin)"
    );
}

/// Installer that succeeds without touching the OS — the success-path
/// twin of `FailingHookInstaller`.
struct SucceedingHookInstaller;
impl HookInstaller for SucceedingHookInstaller {
    fn install(self) -> bool {
        true
    }
}

#[test]
fn run_pump_startup_signals_success_before_writing_diagnostics() {
    // Same deadline hazard on the success path: a slow tee write before
    // the `Ok(())` send would time the caller out into the latch-held
    // branch while a live hook exists, so `install()` reports false for
    // a hook that is actually running.
    let (entered, observed) = outcome_visible_at_first_log(SucceedingHookInstaller);
    assert!(
        entered,
        "a confirmed hook install must enter the message pump - LL-hook \
         callbacks only fire while GetMessageW is running"
    );
    assert_eq!(
        observed,
        Some(true),
        "the Ok outcome must reach the readiness channel before the install \
         marker is written to the tee file"
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
