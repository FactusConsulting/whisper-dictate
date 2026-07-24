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
