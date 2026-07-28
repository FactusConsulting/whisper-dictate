//! Shared binary-entrypoint shell.
//!
//! Both shipping binaries (`whisper-dictate.exe`, `whisper-dictate-gui.exe`)
//! wrap a fallible `Result<()>`-returning closure with the same tiny pattern:
//! print the error to stderr with a fixed prefix on failure and hand the
//! process an appropriate exit code. Splitting that shell out here means:
//!
//! * `fn main()` in each binary stays a one-liner (the smallest possible
//!   untestable entrypoint — Rust cannot unit-test `fn main()` itself), and
//! * the actual behaviour (exit code + stderr message) is a pure, testable
//!   function that both binaries reuse.
//!
//! The stderr writer is generic so tests can pass a `Vec<u8>` and assert on
//! the emitted bytes instead of mocking `std::io::stderr` globally.
//!
//! Coverage rationale: this module keeps the coverage metric honest for the
//! `error_exit_shell` logic. The remaining `fn main()` shells in `main.rs`
//! and `whisper-dictate-gui.rs` are still coverage-excluded because they are
//! literally one call into this helper and cannot be exercised by any Rust
//! unit test.

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

/// How long the shared exit teardown waits for the async diagnostic
/// writer ([`crate::diag::drain_and_shutdown`]) to flush its queue.
///
/// 500 ms, chosen as the smallest value that is unambiguously on the
/// right side of both bounds:
///
/// * **Lower bound - never a false timeout.** The writer's per-record
///   work is two `writeln!` calls plus a `flush`, and the queue holds
///   at most [`crate::diag::ASYNC_QUEUE_CAPACITY`] (256) records, so a
///   healthy drain finishes in single-digit milliseconds. 500 ms is
///   two orders of magnitude of headroom - a CI box under load, or a
///   cold `%LOCALAPPDATA%` first write, still completes well inside
///   it. The same number is already the proven-good budget for
///   `diag::flush_async_for_tests`.
/// * **Upper bound - never a visible stall.** This runs after the UI
///   loop / CLI verb has returned, when the user expects the process
///   to be gone. Half a second of extra teardown is below the
///   threshold where a tray exit reads as "hung"; a multi-second wait
///   on a wedged AppData volume would be worse than the missing log
///   records it is trying to save.
pub const DIAG_DRAIN_DEADLINE: Duration = Duration::from_millis(500);

/// Warning emitted when the exit drain misses [`DIAG_DRAIN_DEADLINE`].
///
/// A named constant so the companion tests assert on the same string
/// production emits, without duplicating the prose. ASCII only - it
/// reaches a console (pinned by `console_ascii_tests`).
pub const DIAG_DRAIN_TIMEOUT_WARNING: &str =
    "[exit] diag async writer drain-and-shutdown deadline expired; pending \
     records may not have landed in the tee file";

/// Run `f`, print its `Err` (if any) to `stderr` prefixed with `prefix`, and
/// return the resulting process exit code.
///
/// * Success (`Ok(())`) → [`ExitCode::SUCCESS`], nothing written.
/// * Failure (`Err(err)`) → [`ExitCode::FAILURE`], one line
///   `"{prefix}: {err}\n"` written to `stderr`.
///
/// The write is best-effort — a stderr write failure is intentionally ignored
/// (we're already on the error path; a second failure just before exiting
/// changes nothing observable).
pub fn error_exit_shell<F, W>(prefix: &str, mut stderr: W, f: F) -> ExitCode
where
    F: FnOnce() -> anyhow::Result<()>,
    W: Write,
{
    match f() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(stderr, "{prefix}: {err}");
            ExitCode::FAILURE
        }
    }
}

/// [`error_exit_shell`] plus the shared process teardown every binary
/// needs. **This is the entrypoint both `fn main`s call.**
///
/// ## Why (Codex P2 #675 PRRT_kwDOSfNjQs6Uc5kn)
///
/// The async diagnostic writer ([`crate::diag::enqueue_async`]) buffers
/// records on a background thread so file I/O stays off the Windows
/// `WH_KEYBOARD_LL` callback. A bare `main` return kills that thread
/// with whatever is still queued, so the queue has to be drained on the
/// way out.
///
/// The abandoned first attempt at this wired the drain into the GUI
/// binary only. But the CLI binary has finite rdev-driven verbs -
/// `self-test hotkey-boot`, `hotkey capture --for-secs ...` - that
/// install the same LL hook, emit the same `raw=` / chord records
/// through the same queue, and then return normally. Those records were
/// discarded at process exit, which is precisely backwards: the CLI
/// verbs exist *because* the operator is capturing a wedge repro from
/// PowerShell.
///
/// Wiring the drain here rather than per-verb means every current and
/// future verb gets it for free, and there is exactly one place where
/// the teardown order (run -> drain -> exit code) is decided.
///
/// The body is deliberately nothing but "hand the real drain to
/// [`error_exit_shell_with_teardown_using`]", so the composition itself
/// is unit-tested (against an injected teardown) and this function has
/// only one thing left for a test to check: that the teardown it
/// injects is the production drain. `entrypoint_tests` pins both halves
/// (Codex P2 #681 PRRT_kwDOSfNjQs6UiJ_P).
pub fn error_exit_shell_with_teardown<F, W>(prefix: &str, stderr: W, f: F) -> ExitCode
where
    F: FnOnce() -> anyhow::Result<()>,
    W: Write,
{
    error_exit_shell_with_teardown_using(prefix, stderr, f, || {
        drain_diagnostics_on_exit();
    })
}

/// Dependency-injected core of [`error_exit_shell_with_teardown`]: run
/// the closure, then the teardown, then hand back the exit code the
/// closure earned.
///
/// The ordering is the contract, and all three parts of it matter:
///
/// * `f` runs FIRST, so the drain flushes records the closure itself
///   queued on its way out.
/// * `teardown` runs UNCONDITIONALLY - a failed run is exactly when the
///   queued trace matters most. "Unconditionally" includes a PANIC, see
///   below.
/// * the exit code survives the teardown: whatever the teardown reports
///   about the diagnostic queue, it must not change what the process
///   tells its caller.
///
/// ## Why the teardown is a `Drop` guard and not a plain call
///
/// Codex P2 #681 comment 3669249183. A straight-line
/// `let code = error_exit_shell(..); teardown(); code` covers exactly
/// two of the three ways this function can be left: `Ok` and `Err`. The
/// third is an unwinding panic inside `f`, and the release profile uses
/// Rust's default unwind behaviour, so an ordinary main-thread panic
/// takes that path. Control then never reaches the `teardown()`
/// statement and the process dies with the queued trace tail unwritten -
/// on precisely the run where a support thread most wants it, because
/// the records nearest the panic are the ones that explain it.
///
/// A scope guard is left on every path, so the drain runs while the
/// stack unwinds. `Option::take` in the `Drop` keeps it exactly-once:
/// the normal path does not drain twice.
///
/// Injecting the teardown is what makes any of this testable: the
/// production drain talks to a process-wide `OnceLock` writer thread
/// that no test can reset, and running it for real inside the test
/// binary would shut that writer down for every later test.
pub(crate) fn error_exit_shell_with_teardown_using<F, W, T>(
    prefix: &str,
    stderr: W,
    f: F,
    teardown: T,
) -> ExitCode
where
    F: FnOnce() -> anyhow::Result<()>,
    W: Write,
    T: FnOnce(),
{
    /// Runs `teardown` when the enclosing scope is left - by `return`
    /// **or** by an unwinding panic out of `f`.
    struct TeardownGuard<T: FnOnce()> {
        /// `Option` so the `FnOnce` can be moved out of `&mut self` in
        /// `Drop`, which also makes "exactly once" structural.
        teardown: Option<T>,
    }

    impl<T: FnOnce()> Drop for TeardownGuard<T> {
        fn drop(&mut self) {
            if let Some(teardown) = self.teardown.take() {
                teardown();
            }
        }
    }

    // Named binding, NOT `let _ = ...`: the latter drops immediately and
    // would run the teardown BEFORE `f`.
    let _teardown_guard = TeardownGuard {
        teardown: Some(teardown),
    };
    error_exit_shell(prefix, stderr, f)
}

/// Drain the async diagnostic queue, warning through a **tee-free**
/// sink if the drain misses its deadline. Production wiring for
/// [`drain_diagnostics_on_exit_with`].
pub fn drain_diagnostics_on_exit() -> bool {
    drain_diagnostics_on_exit_with(
        crate::diag::drain_and_shutdown,
        exit_timeout_warning_sink,
        DIAG_DRAIN_DEADLINE,
    )
}

/// The sink the exit-teardown timeout warning is emitted through.
///
/// A named function rather than an inline closure so a RUNTIME test can
/// drive the role instead of an implementation it picked itself. The
/// injected-core tests below necessarily supply their own `warn`, so
/// without this seam the only thing pinning production's choice would be
/// a source-level string match, and Codex P1 #681
/// PRRT_kwDOSfNjQs6UjZeP is precisely a case where the wrong choice
/// (`crate::diag::write_line_nonblocking`) looks bounded and is not:
/// its `try_lock` succeeds whenever the tee mutex is free, and the
/// synchronous file write behind it can then stall process exit for as
/// long as the AppData volume is wedged.
/// `diag_tests::the_exit_timeout_warning_does_not_write_to_a_free_but_blocked_tee`
/// calls THIS function against exactly that tee.
pub(crate) fn exit_timeout_warning_sink(line: &str) {
    crate::diag::write_line_stderr_only(line);
}

/// Dependency-injected core of [`drain_diagnostics_on_exit`]. Returns
/// whether the drain completed within `deadline`.
///
/// `warn` MUST NOT TOUCH THE TEE AT ALL - not with a blocking `lock`,
/// and not with a `try_lock` either.
///
/// Codex P2 #675 PRRT_kwDOSfNjQs6Ub__j established the first half: the
/// likeliest reason the drain timed out is that the writer thread is
/// wedged INSIDE `crate::diag::write_line_to` still holding the tee-file
/// mutex, so a blocking `diag::log!` here would queue on that same mutex
/// and hang teardown indefinitely - well past the deadline that exists
/// to prevent exactly that.
///
/// Codex P1 #681 PRRT_kwDOSfNjQs6UjZeP established the second: the
/// `try_lock` fallback (`crate::diag::write_line_nonblocking`) is not
/// enough either, because it bounds only the LOCK. When the drain fails
/// while the mutex happens to be free - the writer disconnected, or it
/// released the mutex just before the warning ran - the `try_lock`
/// succeeds and the warning then does a synchronous `writeln!` +
/// `flush` on the same stalled volume, hanging exit inside the warning
/// about the wedged sink. Production therefore wires
/// `crate::diag::write_line_stderr_only`, which has no tee interaction
/// to bound.
pub(crate) fn drain_diagnostics_on_exit_with<D, W>(
    drain: D,
    mut warn: W,
    deadline: Duration,
) -> bool
where
    D: FnOnce(Duration) -> bool,
    W: FnMut(&str),
{
    if drain(deadline) {
        return true;
    }
    warn(DIAG_DRAIN_TIMEOUT_WARNING);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ExitCode does not implement PartialEq, so we can't `assert_eq!` two of
    // them directly. Compare their Debug-formatted string, which serialises
    // both SUCCESS and FAILURE deterministically ("ExitCode(unix_exit_status(0))"
    // etc.) — good enough for the two-value discrimination we care about.
    fn same_code(a: ExitCode, b: ExitCode) -> bool {
        format!("{a:?}") == format!("{b:?}")
    }

    #[test]
    fn success_returns_success_code_and_writes_nothing() {
        let mut stderr = Vec::<u8>::new();
        let code = error_exit_shell("error", &mut stderr, || Ok(()));
        assert!(same_code(code, ExitCode::SUCCESS));
        assert!(
            stderr.is_empty(),
            "stderr should be empty on success, got {:?}",
            String::from_utf8_lossy(&stderr)
        );
    }

    #[test]
    fn failure_returns_failure_code_and_writes_prefixed_line() {
        let mut stderr = Vec::<u8>::new();
        let code = error_exit_shell("error", &mut stderr, || {
            Err(anyhow::anyhow!("something broke"))
        });
        assert!(same_code(code, ExitCode::FAILURE));
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "error: something broke\n",
        );
    }

    #[test]
    fn failure_uses_the_caller_supplied_prefix_verbatim() {
        // Both binaries currently pass "error" but the shell must not hardcode
        // it — a regression that ignored the prefix would leak here.
        let mut stderr = Vec::<u8>::new();
        error_exit_shell("startup", &mut stderr, || Err(anyhow::anyhow!("boom")));
        assert_eq!(String::from_utf8(stderr).unwrap(), "startup: boom\n");
    }

    #[test]
    fn failure_preserves_multi_line_error_display() {
        // anyhow renders context-chained errors as multi-line strings via
        // Display; the shell writes the FULL `err` (Display, not Debug), so
        // the message body must round-trip verbatim (no truncation, no re-
        // wrapping). Guards against a future refactor that swaps `{err}` for
        // `{err:?}` or trims.
        let err = anyhow::anyhow!("outer").context("inner reason");
        let mut stderr = Vec::<u8>::new();
        error_exit_shell("error", &mut stderr, || Err(err));
        let out = String::from_utf8(stderr).unwrap();
        assert!(out.starts_with("error: inner reason"), "got: {out:?}");
    }

    #[test]
    fn closure_result_is_consumed_only_once() {
        // The closure MUST be `FnOnce`, not `Fn` — the shell may only run the
        // caller's work exactly once (side effects, resource acquisition,
        // etc.). Rust's type system already enforces this at compile time via
        // the `FnOnce` bound; this test just documents the contract with a
        // move-into-closure so a signature change to `Fn` fails to compile.
        let owned = String::from("only-once");
        let mut stderr = Vec::<u8>::new();
        let code = error_exit_shell("error", &mut stderr, move || {
            drop(owned); // consumes the moved-in owned value
            Ok(())
        });
        assert!(same_code(code, ExitCode::SUCCESS));
    }

    #[test]
    fn stderr_write_failure_is_swallowed_and_still_returns_failure_code() {
        // If the caller supplies a writer that always errors (e.g. a closed
        // pipe in real life), the shell must NOT propagate the write failure
        // back to the caller — it's already on the error path, and a
        // secondary write error changes nothing observable. Assert the
        // FAILURE exit code still comes through.
        struct AlwaysError;
        impl Write for AlwaysError {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("closed pipe"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("closed pipe"))
            }
        }
        let code = error_exit_shell("error", AlwaysError, || {
            Err(anyhow::anyhow!("original error"))
        });
        assert!(same_code(code, ExitCode::FAILURE));
    }
}
