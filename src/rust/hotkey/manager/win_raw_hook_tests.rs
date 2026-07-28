//! Companion tests for [`crate::hotkey::manager::win_raw_hook`].
//!
//! The pure helpers (rate limiter, `wm=` name mapping, trace-line
//! formatter) are always compiled, so their tests run on every platform
//! CI covers — they exercise plain string / integer logic and never call
//! `SetWindowsHookExW`. Only the genuinely-Windows test (the `install()`
//! gate) carries a `#[cfg(windows)]` of its own.

#![cfg(test)]

use crate::hotkey::manager::win_raw_hook::{
    format_raw_hook_trace_line, should_log_raw_hook_event, wm_message_name, RAW_HOOK_INITIAL_TRACE,
    RAW_HOOK_TRACE_EVERY,
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

#[cfg(windows)]
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
// format_raw_hook_trace_line — the exact grep shape a support thread
// matches, extracted from the LL-hook callback so it is testable at all
// (and, being pure, testable on Linux too).
// ---------------------------------------------------------------------

#[test]
fn format_raw_hook_trace_line_keeps_the_grep_shape() {
    // F9 keydown, extended bit clear, injected bit clear.
    let line = format_raw_hook_trace_line(7, 0x0100, 0x78, 0x43, 0x0000, false, false);
    assert_eq!(
        line,
        "[win/raw-hook] #7 wm=WM_KEYDOWN vk=0x78 scan=0x43 flags=0x0000 \
         injected=false extended=false",
        "the raw-hook trace line is the artefact a support thread greps \
         on a Windows PTT wedge report; its field order and spelling are \
         the contract"
    );
}

#[test]
fn format_raw_hook_trace_line_reports_the_flag_bits() {
    // flags 0x11 = extended (bit 0) + injected (bit 4). The injected
    // bit is the injection-guard regression signal.
    let line = format_raw_hook_trace_line(200, 0x0104, 0xa2, 0x1d, 0x0011, true, true);
    assert!(
        line.contains("wm=WM_SYSKEYDOWN"),
        "SYSKEYDOWN vs KEYDOWN is the interesting distinction for Alt+Fn \
         chords, got {line}"
    );
    assert!(
        line.contains("injected=true") && line.contains("extended=true"),
        "both KBDLLHOOKSTRUCT flag bits must be reported verbatim, got {line}"
    );
    assert!(
        line.contains("flags=0x0011"),
        "the raw flags word must be present so a future bit we do not \
         decode yet is still inspectable, got {line}"
    );
}

/// Regression guard for the LL-hook timeout hazard.
///
/// Un-fixed behaviour: the callback called `crate::diag::log!`, which
/// takes the tee-file mutex and blocks on an AppData write from inside
/// the `WH_KEYBOARD_LL` callback. Windows unhooks a low-level hook that
/// overruns its few-millisecond budget — so the diagnostic could cause
/// a second instance of the exact wedge it was built to measure.
/// PR #668 rewired the parallel rdev callbacks onto the bounded queue
/// but missed this one.
///
/// Structural rather than runtime: the callback is an
/// `extern "system"` fn the OS calls: there is no way to invoke it from
/// a test without installing a real hook.
#[test]
fn raw_hook_callback_never_writes_synchronously() {
    let body = crate::diag_tests::scan_fn_body(
        "src/rust/hotkey/manager/win_raw_hook.rs",
        "unsafe extern \"system\" fn ll_keyboard_hook_proc(",
    );
    assert!(
        !body.code.contains("crate::diag::log!"),
        "the LL-hook callback must NEVER call the synchronous \
         `crate::diag::log!` - it blocks on the tee-file mutex inside a \
         callback Windows will unhook for overrunning its time budget. \
         Use `crate::diag::enqueue_async` / `log_async!`. Offending \
         function body:\n{}",
        body.raw
    );
    assert!(
        body.code.contains("enqueue_async"),
        "the LL-hook callback must route its trace through the bounded \
         off-callback queue. Offending function body:\n{}",
        body.raw
    );
    assert!(
        body.code.contains("format_raw_hook_trace_line"),
        "the callback must use the pure formatter so the line shape stays \
         testable off-Windows. Offending function body:\n{}",
        body.raw
    );
}

/// The queue is a silent no-op until `ensure_async_writer` populates
/// `ASYNC_QUEUE_TX`. This module installs from `whisper-dictate-gui::
/// main`, which on a stock (non-`rust-hotkeys`) build never reaches
/// `manager_channel()` — the only other install site. Without priming
/// here, moving the callback onto the queue would trade a blocking
/// write for NO write at all.
#[test]
fn install_primes_the_async_writer_before_hooking() {
    let body = crate::diag_tests::scan_fn_body(
        "src/rust/hotkey/manager/win_raw_hook.rs",
        "pub fn install() -> bool {",
    );
    assert!(
        body.code.contains("ensure_async_writer"),
        "install() must prime the off-callback writer before the hook can \
         fire, otherwise every queued raw-hook trace is dropped on a \
         stock build. Offending function body:\n{}",
        body.raw
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
