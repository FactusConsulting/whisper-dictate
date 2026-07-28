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
/// writer to flush its queue.
///
/// Generous next to the sub-millisecond backlogs the queue normally
/// carries, and short enough to bound teardown latency when the writer
/// is stuck (a wedged AppData volume during process exit is exactly
/// the scenario the deadline exists for).
pub const DIAG_DRAIN_DEADLINE: Duration = Duration::from_millis(500);

/// Warning emitted when the exit drain misses [`DIAG_DRAIN_DEADLINE`].
/// Named so the companion tests can assert on it without duplicating
/// the prose.
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
/// The async diagnostic writer ([`crate::diag_async`]) buffers records
/// on a background thread so file I/O stays off the Windows
/// `WH_KEYBOARD_LL` callback. A bare `main` return kills that thread
/// with whatever is still queued, so the queue has to be drained on
/// the way out.
///
/// The drain was originally wired into `whisper-dictate-gui.rs` only.
/// But the CLI binary has finite rdev-driven verbs — `self-test
/// hotkey-boot`, `hotkey capture --for-secs ...` — that install the
/// same LL hook, emit the same `raw=`/chord records through the same
/// queue, and then return normally. Those records were discarded at
/// process exit, which is precisely backwards: the CLI verbs exist
/// *because* the operator is capturing a wedge repro from PowerShell
/// where stderr is visible.
///
/// Wiring the drain here rather than per-verb means every current and
/// future verb gets it for free, and there is exactly one place where
/// the teardown order (run → drain → exit code) is decided.
pub fn error_exit_shell_with_teardown<F, W>(prefix: &str, stderr: W, f: F) -> ExitCode
where
    F: FnOnce() -> anyhow::Result<()>,
    W: Write,
{
    let code = error_exit_shell(prefix, stderr, f);
    drain_diagnostics_on_exit();
    code
}

/// Drain the async diagnostic queue, warning (non-blockingly) if the
/// drain misses its deadline. Production wiring for
/// [`drain_diagnostics_on_exit_with`].
pub fn drain_diagnostics_on_exit() -> bool {
    drain_diagnostics_on_exit_with(
        crate::diag_async::drain_and_shutdown,
        |line| {
            // Discard the "did the tee write land" bool — on this path
            // stderr already has the line and there is nothing further
            // to do about a contended tee mutex.
            crate::diag::write_line_nonblocking(line);
        },
        DIAG_DRAIN_DEADLINE,
    )
}

/// Dependency-injected core of [`drain_diagnostics_on_exit`]. Returns
/// whether the drain completed within `deadline`.
///
/// `warn` MUST be a non-blocking sink. Codex P2 #675
/// PRRT_kwDOSfNjQs6Ub__j: the likeliest reason the drain timed out is
/// that the writer thread is wedged INSIDE `crate::diag::write_line`
/// still holding the tee-file mutex, so a blocking `diag::log!` here
/// would queue on that same mutex and hang teardown indefinitely —
/// well past the deadline that exists to prevent exactly that.
/// [`crate::diag::write_line_nonblocking`] `try_lock`s and falls back
/// to stderr-only.
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    // -------------------------------------------------------------------
    // Codex P2 #675 PRRT_kwDOSfNjQs6Uc5kn — drain diagnostics before the
    // CLI process exits, via SHARED exit wiring rather than per-verb.
    // -------------------------------------------------------------------

    /// The load-bearing regression: **both** shipping binaries must
    /// route their `fn main` through [`error_exit_shell_with_teardown`],
    /// not the bare [`error_exit_shell`].
    ///
    /// `fn main` is the one thing Rust cannot unit-test, so this pins
    /// the wiring at the source level — the same technique
    /// `src/rust/tests/manual_test_docs.rs` uses for docs. That is
    /// exactly the granularity of Codex's finding: "a repo-wide search
    /// finds the sole `drain_and_shutdown` call in
    /// `whisper-dictate-gui.rs`, while `src/rust/main.rs:20-22` returns
    /// directly from `error_exit_shell`".
    ///
    /// FAILS on the un-fixed tree: `main.rs` contains a bare
    /// `error_exit_shell(` call and no teardown wrapper, so every
    /// record a finite rdev verb queued dies with the writer thread.
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
                 diag queue is drained before the process exits (Codex P2 #675 \
                 PRRT_kwDOSfNjQs6Uc5kn)"
            );
            // `error_exit_shell_with_teardown(` does not contain
            // `error_exit_shell(` as a substring (the next char is `_`),
            // so this cleanly catches a binary that reverted to the
            // undrained shell.
            assert!(
                !src.contains("error_exit_shell("),
                "{binary} still calls the bare `error_exit_shell` — that path \
                 returns from main without draining the diagnostic queue, \
                 discarding every record a finite rdev verb enqueued"
            );
        }
    }

    /// The composed teardown must not swallow the exit code, and must
    /// run the drain exactly once for either outcome of the wrapped
    /// closure. Uses the injected core so the test never touches the
    /// process-wide `diag_async` writer slot (an `OnceLock` a test
    /// cannot reset).
    #[test]
    fn teardown_drains_once_on_both_success_and_failure() {
        for (label, outcome, expected) in [
            ("success", Ok(()), ExitCode::SUCCESS),
            ("failure", Err(anyhow::anyhow!("boom")), ExitCode::FAILURE),
        ] {
            let drains = AtomicUsize::new(0);
            let mut stderr = Vec::<u8>::new();
            let code = error_exit_shell("error", &mut stderr, move || outcome);
            drain_diagnostics_on_exit_with(
                |_deadline| {
                    drains.fetch_add(1, Ordering::SeqCst);
                    true
                },
                |_line| panic!("must not warn when the drain completed in time"),
                DIAG_DRAIN_DEADLINE,
            );
            assert!(same_code(code, expected), "{label}: wrong exit code");
            assert_eq!(
                drains.load(Ordering::SeqCst),
                1,
                "{label}: the exit teardown must drain exactly once"
            );
        }
    }

    /// A drain that misses its deadline must warn — and must warn
    /// through the caller-supplied sink, which production wires to the
    /// NON-blocking `diag::write_line_nonblocking`. A regression that
    /// silently swallowed the timeout would leave an operator believing
    /// a wedge repro landed in the tee file when it did not.
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

    /// The deadline must be a real bound: long enough that an ordinary
    /// sub-millisecond backlog always flushes, short enough that a
    /// wedged AppData volume cannot pin process exit.
    #[test]
    fn diag_drain_deadline_bounds_teardown_latency() {
        assert!(
            DIAG_DRAIN_DEADLINE >= Duration::from_millis(100),
            "too short — an ordinary backlog would be reported as a timeout"
        );
        assert!(
            DIAG_DRAIN_DEADLINE <= Duration::from_secs(2),
            "too long — a wedged writer would visibly stall process exit"
        );
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
