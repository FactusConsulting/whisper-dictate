//! Process-wide async writer startup and observable spawn outcome.

use super::{
    run_async_writer_loop, write_line, AsyncRecord, ASYNC_DROPPED, ASYNC_PENDING,
    ASYNC_QUEUE_CAPACITY, ASYNC_QUEUE_TX,
};
use std::sync::{mpsc, OnceLock};
use std::thread;

/// Recorded reason the off-callback writer thread never came up, or
/// unset when it did. Written exactly once, from inside
/// [`ensure_async_writer`]'s `OnceLock` initialiser, so it is settled by
/// the time that call returns for ANY thread.
///
/// Before this existed, [`ensure_async_writer`] swallowed the
/// `thread::Builder::spawn` `Err` outright: the sender was installed
/// regardless, so `async_writer_installed()` reported `true`, every
/// `enqueue_async` call filled a queue nobody was reading, and once the
/// 256 slots were gone every callback-path diagnostic - the rdev
/// boundary trace, the tracker `[chord]` trace, the Windows raw-hook
/// trace - vanished with no line anywhere saying why. A process in that
/// state looks exactly like a healthy quiet one in `gui-diagnostic.log`.
static ASYNC_WRITER_SPAWN_ERROR: OnceLock<String> = OnceLock::new();

/// Render `text` as printable ASCII on ONE line, escaping everything
/// else as `\u{...}`.
///
/// ## Why an OS error cannot be interpolated raw
///
/// `console_ascii_tests` is a scan of
/// source LITERALS: it proves the prose we wrote is ASCII, and it is
/// blind to whatever `{err}` expands to at runtime. A
/// [`std::io::Error`] from `thread::Builder::spawn` is OS-derived, and
/// `FormatMessageW` returns the system-locale text - on a Danish,
/// German, Japanese or Russian Windows the rendered message carries
/// non-ASCII. That string then reaches stderr through [`write_line`],
/// where a cmd.exe on a legacy code page renders it as mojibake, so the
/// one line explaining why the diagnostic pipeline is dead is itself
/// unreadable. An embedded newline would be worse still: it would split
/// one record into two and break the one-line-per-record grep contract
/// the whole tee file is read with.
///
/// Escaping rather than dropping: the escape is lossless and
/// round-trippable, so a support thread can still recover the original
/// localized text from the log when it matters, while the bytes that
/// actually reach the console stay in `0x20..=0x7e`.
pub(crate) fn ascii_escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        // Printable ASCII passes through untouched; DEL, the C0
        // controls (newline and tab included) and every non-ASCII
        // scalar become an escape.
        if c.is_ascii_graphic() || c == ' ' {
            out.push(c);
        } else {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        }
    }
    out
}

/// The message recorded (and logged) when the off-callback writer
/// thread fails to spawn.
///
/// A named function rather than an inline `format!` so the regression
/// test can assert the shape without duplicating the format string.
///
/// ASCII only: this string reaches stderr via [`write_line`] and
/// typographic punctuation renders as mojibake under cmd.exe on a
/// legacy code page (AGENTS.md; pinned by `console_ascii_tests`). The
/// prose here is covered by that source scan; the OS-derived `err` is
/// NOT (it does not exist until runtime), so it goes through
/// [`ascii_escaped`] - see that function for why.
pub(crate) fn writer_spawn_failure_message(err: &std::io::Error) -> String {
    let err = ascii_escaped(&err.to_string());
    format!(
        "[diag-async] writer thread spawn failed: {err} - callback-path \
         diagnostics (rdev boundary trace, chord trace, raw-hook trace) \
         cannot be written for the rest of this process"
    )
}

/// Record the writer thread's spawn outcome into `slot`.
///
/// `Ok` leaves the slot untouched (the absence of a recorded message IS
/// "the writer is running"); `Err` stores the formatted
/// [`writer_spawn_failure_message`]. Parameterised over the slot and
/// generic over the spawn payload so `diag_tests` can drive both arms
/// against a local `OnceLock` — a real `thread::Builder::spawn` failure
/// cannot be provoked in a unit test.
pub(crate) fn record_writer_spawn_outcome<T>(
    slot: &OnceLock<String>,
    spawned: std::io::Result<T>,
) -> Result<(), String> {
    match spawned {
        Ok(_) => Ok(()),
        Err(err) => {
            let msg = writer_spawn_failure_message(&err);
            let _ = slot.set(msg.clone());
            Err(msg)
        }
    }
}

/// Idempotently install the off-callback trace writer thread. Safe to
/// call from every callback-path caller because the writer is gated by
/// [`OnceLock`]; only the first caller spawns the thread, every
/// subsequent caller no-ops.
///
/// A spawn failure is NOT swallowed any more: it is recorded in
/// [`ASYNC_WRITER_SPAWN_ERROR`] and surfaced by
/// [`async_writer_result`], which the rdev listener consults before it
/// announces readiness.
pub fn ensure_async_writer() {
    ASYNC_QUEUE_TX.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<AsyncRecord>(ASYNC_QUEUE_CAPACITY);
        // Named for `taskkill /t` traces and future panic-hook attribution.
        let spawned = thread::Builder::new()
            .name("vp-hotkey-diag-async".to_owned())
            .spawn(move || {
                run_async_writer_loop(
                    rx,
                    ASYNC_QUEUE_CAPACITY,
                    &ASYNC_PENDING,
                    &ASYNC_DROPPED,
                    write_line,
                );
            });
        // Still install the sender on failure (callers must keep working
        // and `enqueue_async` degrades to a silent drop), but REMEMBER
        // why, so the listener can report a dead diagnostic pipeline
        // instead of the process discovering it as missing log lines
        // hours later.
        if let Err(msg) = record_writer_spawn_outcome(&ASYNC_WRITER_SPAWN_ERROR, spawned) {
            write_line(&msg);
        }
        tx
    });
}

/// Install the off-callback writer (idempotently, via
/// [`ensure_async_writer`]) and report whether it is actually running.
///
/// `Ok(())` means a writer thread is draining the queue. `Err(msg)`
/// means the thread never spawned and every callback-path diagnostic
/// will be dropped for the rest of the process — the rdev listener maps
/// that to `SpawnError::WriterStartup` so `install_hotkey()` fails loudly
/// rather than running blind.
pub fn async_writer_result() -> Result<(), String> {
    ensure_async_writer();
    match ASYNC_WRITER_SPAWN_ERROR.get() {
        Some(msg) => Err(msg.clone()),
        None => Ok(()),
    }
}

/// True once [`ensure_async_writer`] has populated the process-wide
/// sender. Callers on the LL-hook path never need this (they
/// fire-and-forget through [`enqueue_async`]); it exists so a
/// regression test can assert that a backend's install path actually
/// installed the writer, rather than silently dropping every queued
/// trace.
pub fn async_writer_installed() -> bool {
    ASYNC_QUEUE_TX.get().is_some()
}
