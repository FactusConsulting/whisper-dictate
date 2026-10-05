//! Diagnostic companion-test registry and shared structural/sink fixtures.

use crate::diag_test_lock::DIAG_WRITER_LOCK;
use std::sync::{Condvar, Mutex, MutexGuard};

#[path = "diag/queue_support_tests.rs"]
mod queue_support_tests;
use queue_support_tests::{
    drive_a_saturated_async_queue, flood_a_stalled_async_queue, reported_drop_counts,
};

#[path = "diag/config_tests.rs"]
mod config_tests;
#[path = "diag/exit_warning_tests.rs"]
mod exit_warning_tests;
#[path = "diag/logger_tests.rs"]
mod logger_tests;
#[path = "diag/panic_tests.rs"]
mod panic_tests;
#[path = "diag/queue_tests.rs"]
mod queue_tests;
#[path = "diag/shutdown_protocol_tests.rs"]
mod shutdown_protocol_tests;
#[path = "diag/shutdown_tests.rs"]
mod shutdown_tests;
#[path = "diag/startup_tests.rs"]
mod startup_tests;
#[path = "diag/writer_tests.rs"]
mod writer_tests;

#[test]
fn diagnostic_facade_keeps_the_public_startup_and_drain_signatures() {
    let _: fn() -> crate::diag::LogLevel = crate::diag::init_from_env;
    let _: fn() -> Result<(), String> = crate::diag::async_writer_result;
    let _: fn(String) = crate::diag::enqueue_async;
    let _: fn(std::time::Duration) -> bool = crate::diag::drain_and_shutdown;
    let _: fn(std::time::Duration) -> bool = crate::diag::drain_panic_reports;
    let _: fn(&str) = crate::diag::write_line;
    let _: fn(&str) = crate::diag::write_line_stderr_only;
    assert_eq!(crate::diag::ASYNC_QUEUE_CAPACITY, 256);
}

#[test]
fn diagnostic_modules_keep_exactly_one_shared_queue_gate_and_ledger() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut source = std::fs::read_to_string(root.join("diag.rs")).unwrap();
    for module in [
        "config", "logger", "panic", "queue", "shutdown", "startup", "writer",
    ] {
        source.push_str(
            &std::fs::read_to_string(root.join("diag").join(format!("{module}.rs"))).unwrap(),
        );
    }
    for owner in [
        "ASYNC_QUEUE_TX",
        "ASYNC_GATE",
        "ASYNC_DROPPED",
        "ASYNC_PENDING",
        "PANIC_QUEUE_TX",
    ] {
        assert_eq!(
            source.matches(&format!("static {owner}:")).count(),
            1,
            "{owner} must have one process-wide owner"
        );
    }
    assert_eq!(source.matches("fn diag_file()").count(), 1);
}

/// Serialise diag-mutation tests so parallel runs don't race the
/// process-wide writer slot. Consolidated onto the crate-wide
/// [`DIAG_WRITER_LOCK`] in `crate::diag_test_lock`; the shared lock prevents
/// writer-installation races between diagnostic test modules.
/// `OnceLock<Mutex<()>>` was a different mutex from the identically
/// named lock in `hotkey::manager::tracker_tests`, so tests in the
/// two modules could still race the writer install even though each
/// suite serialised internally.
fn diag_test_lock() -> MutexGuard<'static, ()> {
    DIAG_WRITER_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

// -----------------------------------------------------------------------
// Stderr writes must remain fallible when the consumer closes the stream.
//
// The stderr side of the tee used to be `eprintln!`, which panics on
// `write_all` failure. On Windows the hidden-subsystem launcher / a
// consumer closing a redirected pipe can leave stderr in exactly that
// "closed / invalid" state — the unconditional session marker at
// startup would then abort startup, or a later diagnostic would kill
// the calling thread, losing the very file record intended to diagnose
// the failure. The fix routes the stderr write through
// `io::stderr().lock()` + `writeln!` + `let _ =` so the Err is
// swallowed and the file-append side below still runs.
//
// The exact bug is a Windows-only panic that Linux CI cannot
// reproduce (there is no test-friendly way to close fd 2 from inside
// the same process without polluting other tests' output). The
// regression test therefore pins the STRUCTURE of the fix: the
// production `write_line` function's source must not use `eprintln!`
// for the stderr tee. Un-fixed code contained `eprintln!("{line}")`
// and this assertion would fail.
// -----------------------------------------------------------------------

/// One function body lifted out of a production source file, for the
/// structural scanners below.
///
/// `code` has `//` line comments stripped — assert *absence* of a
/// pattern against this field, so a mention inside the fix's own
/// rationale comment cannot false-fail the check. `raw` keeps the
/// comments — assert *presence* against this one, and use it in
/// failure messages so the operator sees the real source.
///
/// `pub(crate)` because the same mechanism guards the Windows raw-hook
/// callback in `win_raw_hook_tests.rs` — that callback is an
/// `extern "system"` fn only the OS can invoke, so a structural scan is
/// the only way to pin "never write synchronously from inside an
/// LL-hook callback".
pub(crate) struct FnBody {
    pub(crate) raw: String,
    pub(crate) code: String,
}

/// Read `rel_path` and return the body of the function introduced by
/// `fn_marker` (the literal signature text up to and including its
/// opening `{`).
///
/// Extracted so the three structural scanners below share one
/// implementation instead of repeating the read + brace-walk +
/// comment-strip mechanism verbatim. Only the *mechanism* is shared:
/// which file, which function, which pattern and which failure message
/// — everything that documents what each test guards — stays at the
/// call site.
///
/// The walk is a brace-depth counter rather than a real parse because
/// `syn` is not a project dependency: start at the opening `{`, advance
/// until the matching `}` at depth 0. None of the scanned functions
/// contain unbalanced braces inside string/char literals today; a
/// future refactor that introduced one would need to update this
/// helper, which is acceptable — the point of these tests is structural
/// discipline, not a general-purpose Rust parser.
pub(crate) fn scan_fn_body(rel_path: &str, fn_marker: &str) -> FnBody {
    // `cargo test` runs from the crate root (src/rust), but some
    // invocations run from the repo root — try both.
    let src = std::fs::read_to_string(rel_path)
        .or_else(|_| std::fs::read_to_string(rel_path.trim_start_matches("src/rust/")))
        .unwrap_or_else(|err| {
            panic!("{rel_path} must be readable from the test working dir ({err})")
        });
    let fn_start = src
        .find(fn_marker)
        .unwrap_or_else(|| panic!("{fn_marker:?} must exist in {rel_path}"));
    // The marker may or may not include the opening brace; locate it
    // either way so callers can pass a truncated signature.
    let open_brace_offset = src[fn_start..]
        .find('{')
        .map(|i| fn_start + i)
        .unwrap_or_else(|| panic!("{fn_marker:?} in {rel_path} must have an opening brace"));
    let mut depth: i32 = 0;
    let mut end: Option<usize> = None;
    for (i, ch) in src[open_brace_offset..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open_brace_offset + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let fn_end =
        end.unwrap_or_else(|| panic!("{fn_marker:?} in {rel_path} must have a matching `}}`"));
    let raw = src[fn_start..fn_end].to_owned();
    let code = raw
        .lines()
        .map(|line| line.find("//").map_or(line, |idx| &line[..idx]))
        .collect::<Vec<_>>()
        .join("\n");
    FnBody { raw, code }
}

/// A writer whose every `write` / `flush` fails with `BrokenPipe`
/// the exact `io::Error` a closed redirected-stderr consumer produces
/// on both Windows and Unix. Counts attempts so the test can prove the
/// sink really tried to write rather than skipping the branch.
struct FailingWriter {
    write_attempts: std::cell::Cell<usize>,
}

impl FailingWriter {
    fn new() -> Self {
        Self {
            write_attempts: std::cell::Cell::new(0),
        }
    }
}

impl std::io::Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        self.write_attempts.set(self.write_attempts.get() + 1);
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "simulated closed stderr consumer",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "simulated closed stderr consumer",
        ))
    }
}

// -----------------------------------------------------------------------
// Drop accounting for the off-callback async queue.
//
// `enqueue_async` sheds on a full queue (it must — blocking would park
// the Windows LL-hook callback and re-create the wedge the queue exists
// to prevent). Before this landed the shed was SILENT: a reader of
// `gui-diagnostic.log` saw the same thing whether the callback was
// quiet or whether the queue had thrown away a burst, which makes the
// trace untrustworthy on exactly the slow-AppData scenario it is meant
// to diagnose. The fix counts drops and has the writer thread announce
// them as ONE coalesced `[diag-async] dropped=<n>` line before the next
// record it writes.
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// Marker coalescing across a whole overload burst
// 3668174780).
//
// The shed count rides on the first record accepted after the gap, which
// is right for POSITION but, on its own, wrong for VOLUME: under a
// sustained overload (the documented `VOICEPI_LOG=debug` mouse stream
// against a stalled AppData volume) every dequeue frees exactly one slot,
// the producer refills it at once, and more records are shed while the
// writer is still writing — so every record carries a non-zero count and
// the writer emits nearly ONE MARKER PER SURVIVING RECORD. That doubles
// the write volume against the sink that was already too slow.
//
// The two tests below bound the two halves: a lock-step concurrent
// producer/writer for the amplification itself (the prefilled,
// closed-channel harness above structurally cannot expose it — it has no
// producer running while the writer writes), and a fully synchronous one
// for the episode state machine's exact output.
// -----------------------------------------------------------------------

/// A tee sink whose mutex is perfectly acquirable and whose `write`
/// NEVER RETURNS until the test says so.
///
/// This verifies the lock ordering and the
/// one no `tempfile` can produce: a stalled AppData volume does not hold
/// the `Mutex`, it holds the *syscall*. `try_lock` bounds the former and
/// says nothing about the latter, which is why the exit-teardown warning
/// had to stop touching the tee altogether rather than merely stop
/// blocking on its lock.
struct BlockedTee {
    gate: std::sync::Arc<(Mutex<bool>, Condvar)>,
}

impl std::io::Write for BlockedTee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let (lock, cv) = &*self.gate;
        let mut released = lock.lock().unwrap_or_else(|p| p.into_inner());
        while !*released {
            released = cv.wait(released).unwrap_or_else(|p| p.into_inner());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Release a [`BlockedTee`]'s gate so the stalled write can complete.
fn release_gate(gate: &(Mutex<bool>, Condvar)) {
    let (lock, cv) = gate;
    *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
    cv.notify_all();
}
