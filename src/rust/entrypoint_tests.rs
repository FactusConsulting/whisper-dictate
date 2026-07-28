//! Companion tests for the shared exit teardown in
//! [`crate::entrypoint`].
//!
//! The pre-existing `error_exit_shell` unit tests stay inline in
//! `entrypoint.rs`; this file covers the drain-on-exit wiring added on
//! top of it, because the load-bearing assertion here is a SOURCE-LEVEL
//! pin on both `fn main`s - the one thing Rust cannot unit-test - and
//! that reads better next to the behavioural tests than buried in the
//! module it inspects.
//!
//! Background: the async diagnostic writer is a background thread. A
//! bare `main` return kills it with whatever is still queued, so the
//! records closest to the moment of interest (the tail of a PTT wedge
//! repro) are exactly the ones a support thread never gets to read.

use std::cell::RefCell;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::entrypoint::{
    drain_diagnostics_on_exit_with, error_exit_shell_with_teardown_using, DIAG_DRAIN_DEADLINE,
    DIAG_DRAIN_TIMEOUT_WARNING,
};

// ExitCode does not implement PartialEq, so we can't `assert_eq!` two of
// them directly. Compare their Debug-formatted string, which serialises
// both SUCCESS and FAILURE deterministically.
fn same_code(a: ExitCode, b: ExitCode) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

/// The load-bearing regression: **both** shipping binaries must route
/// their `fn main` through `error_exit_shell_with_teardown`, not the
/// bare `error_exit_shell`.
///
/// `fn main` is the one thing Rust cannot unit-test, so this pins the
/// wiring at the source level - the same technique
/// `src/rust/tests/manual_test_docs.rs` uses for the docs. It is also
/// exactly the granularity of the Codex P2 that motivated it: "a
/// repo-wide search finds the sole `drain_and_shutdown` call in
/// `whisper-dictate-gui.rs`, while `src/rust/main.rs` returns directly
/// from `error_exit_shell`" - i.e. the CLI's finite rdev verbs
/// (`self-test hotkey-boot`, `hotkey capture`) exit undrained.
///
/// FAILS on the un-fixed tree: both binaries contain a bare
/// `error_exit_shell(` call and no teardown wrapper.
#[test]
fn both_binaries_drain_diagnostics_through_the_shared_exit_shell() {
    for (binary, src) in [
        ("main.rs", include_str!("main.rs")),
        (
            "whisper-dictate-gui.rs",
            include_str!("whisper-dictate-gui.rs"),
        ),
    ] {
        assert!(
            src.contains("error_exit_shell_with_teardown("),
            "{binary} must call `error_exit_shell_with_teardown` so the async \
             diag queue is drained before the process exits; otherwise every \
             record a finite rdev verb queued dies with the writer thread"
        );
        // `error_exit_shell_with_teardown(` does not contain
        // `error_exit_shell(` as a substring (the next char is `_`), so
        // this cleanly catches a binary that reverted to the undrained
        // shell.
        assert!(
            !src.contains("error_exit_shell("),
            "{binary} still calls the bare `error_exit_shell` - that path \
             returns from main without draining the diagnostic queue, \
             discarding every record still in flight"
        );
    }
}

/// Structural companion: the production drain must warn through the
/// NON-blocking sink. A regression that reached for `crate::diag::log!`
/// would deadlock teardown in the one scenario the deadline exists for
/// (the writer wedged inside `write_line_to` holding the tee mutex),
/// and no runtime test can observe a hang without hanging CI itself.
#[test]
fn production_exit_drain_warns_through_the_nonblocking_sink() {
    let src = include_str!("entrypoint.rs");
    let body = src
        .split_once("pub fn drain_diagnostics_on_exit() -> bool {")
        .expect("drain_diagnostics_on_exit must exist")
        .1;
    let body = body.split_once("\n}").expect("function must terminate").0;
    assert!(
        body.contains("write_line_nonblocking"),
        "the post-drain warning must go through \
         `diag::write_line_nonblocking`; a blocking `diag::log!` waits on \
         the very tee mutex the wedged writer is holding. Offending body:\n{body}"
    );
    assert!(
        body.contains("crate::diag::drain_and_shutdown"),
        "the production teardown must call the real \
         `diag::drain_and_shutdown`, not a stub. Offending body:\n{body}"
    );
}

/// The COMPOSED teardown - `error_exit_shell_with_teardown`'s own body,
/// reached through its injected core - must run the drain exactly once,
/// after the wrapped closure, for either outcome, and must not swallow
/// the exit code.
///
/// Codex P2 #681 PRRT_kwDOSfNjQs6UiJ_P: the previous shape called
/// `error_exit_shell` and `drain_diagnostics_on_exit_with` as two
/// unrelated statements, so it asserted only that each half works in
/// isolation - deleting the drain from the production wrapper left it
/// green. Driving the composition itself is what pins the runtime
/// ordering; its companion below pins which teardown production
/// injects.
///
/// The teardown stays injected: the real drain talks to a process-wide
/// `OnceLock` writer that other tests in this binary install and that
/// no test can reset, so running it here would stop that writer for
/// every later test.
///
/// Un-fixed behaviour (a core that drops its `teardown()` call, or runs
/// it before `f`): the recorded order is `["run"]` / `["drain", "run"]`
/// instead of `["run", "drain"]`.
#[test]
fn the_composed_exit_shell_drains_once_after_the_closure_on_both_outcomes() {
    for (label, outcome, expected) in [
        ("success", Ok(()), ExitCode::SUCCESS),
        ("failure", Err(anyhow::anyhow!("boom")), ExitCode::FAILURE),
    ] {
        let order = RefCell::new(Vec::<&'static str>::new());
        let drains = AtomicUsize::new(0);
        let mut stderr = Vec::<u8>::new();

        let code = error_exit_shell_with_teardown_using(
            "error",
            &mut stderr,
            || {
                order.borrow_mut().push("run");
                outcome
            },
            || {
                order.borrow_mut().push("drain");
                drain_diagnostics_on_exit_with(
                    |_deadline| {
                        drains.fetch_add(1, Ordering::SeqCst);
                        true
                    },
                    |_line| panic!("must not warn when the drain completed in time"),
                    DIAG_DRAIN_DEADLINE,
                );
            },
        );

        assert!(same_code(code, expected), "{label}: wrong exit code");
        assert_eq!(
            *order.borrow(),
            vec!["run", "drain"],
            "{label}: the exit shell must run the wrapped closure first and \
             drain afterwards, so records the closure queued on its way out \
             still reach the tee file"
        );
        assert_eq!(
            drains.load(Ordering::SeqCst),
            1,
            "{label}: the exit teardown must drain exactly once"
        );
    }
}

/// Companion to the composition test above: the production wrapper must
/// inject the REAL drain.
///
/// The runtime test cannot check this - it supplies its own teardown on
/// purpose - so the one remaining line of `error_exit_shell_with_teardown`
/// is pinned at the source level, the same technique the sibling test
/// uses on `drain_diagnostics_on_exit`. Together they close the hole
/// Codex P2 #681 PRRT_kwDOSfNjQs6UiJ_P named: with only the source-level
/// `fn main` pin, deleting the drain from this wrapper left every test
/// green while shipping binaries that exit undrained.
///
/// Un-fixed behaviour (wrapper body reduced to `|| {}`): panics with
/// "must hand the production drain".
#[test]
fn the_production_exit_shell_injects_the_real_diagnostic_drain() {
    let src = include_str!("entrypoint.rs");
    // `error_exit_shell_with_teardown_using` has a three-parameter
    // generic list, so this needle matches the wrapper only.
    let body = src
        .split_once("pub fn error_exit_shell_with_teardown<F, W>(")
        .expect("error_exit_shell_with_teardown must exist")
        .1;
    let body = body.split_once("\n}").expect("function must terminate").0;
    assert!(
        body.contains("error_exit_shell_with_teardown_using"),
        "the production exit shell must route through the injected core \
         so the composition itself stays unit-tested. Offending body:\n{body}"
    );
    assert!(
        body.contains("drain_diagnostics_on_exit()"),
        "the production exit shell must hand the production drain \
         `drain_diagnostics_on_exit` to its teardown core; a no-op teardown \
         here ships binaries whose finite rdev verbs exit with the async \
         diagnostic queue undrained. Offending body:\n{body}"
    );
}

/// A drain that misses its deadline must warn - and must warn through
/// the caller-supplied sink, which production wires to the non-blocking
/// `diag::write_line_nonblocking`. A regression that silently swallowed
/// the timeout would leave an operator believing a wedge repro landed
/// in the tee file when it did not.
#[test]
fn teardown_warns_when_the_drain_misses_its_deadline() {
    let mut warnings = Vec::<String>::new();
    let completed = drain_diagnostics_on_exit_with(
        |deadline| {
            assert_eq!(
                deadline, DIAG_DRAIN_DEADLINE,
                "the shared deadline must be passed through verbatim"
            );
            false
        },
        |line| warnings.push(line.to_owned()),
        DIAG_DRAIN_DEADLINE,
    );
    assert!(!completed, "a timed-out drain must report false");
    assert_eq!(
        warnings,
        vec![DIAG_DRAIN_TIMEOUT_WARNING.to_owned()],
        "exactly one timeout warning, using the shared constant"
    );
}

/// The deadline must stay a real bound: long enough that an ordinary
/// sub-millisecond backlog always flushes, short enough that a wedged
/// AppData volume cannot pin process exit.
#[test]
fn diag_drain_deadline_bounds_teardown_latency() {
    assert!(
        DIAG_DRAIN_DEADLINE >= Duration::from_millis(100),
        "too short - an ordinary backlog would be reported as a timeout"
    );
    assert!(
        DIAG_DRAIN_DEADLINE <= Duration::from_secs(2),
        "too long - a wedged writer would visibly stall process exit"
    );
}
