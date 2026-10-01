//! startup diagnostic regressions.

use super::{diag_test_lock, scan_fn_body};

// -----------------------------------------------------------------------
// Writer-spawn failure accounting.
//
// `ensure_async_writer` used to do `let _ = thread::Builder::new()...
// .spawn(...)`. On a spawn failure the sender was installed anyway, so
// `async_writer_installed()` reported `true`, `enqueue_async` kept
// filling a queue nobody was reading, and once the 256 slots were gone
// every callback-path diagnostic vanished with nothing anywhere saying
// why. A process in that state is indistinguishable from a healthy quiet
// one in `gui-diagnostic.log`, which is the worst possible failure mode
// for a file whose entire job is post-hoc diagnosis.
//
// A real `thread::Builder::spawn` failure cannot be provoked in a unit
// test, so the recording half is parameterised over the slot and the
// spawn result; the structural scanner below pins that production is
// wired to it.
// -----------------------------------------------------------------------

#[test]
fn record_writer_spawn_outcome_records_a_failed_spawn() {
    let slot: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let err = std::io::Error::other("Resource temporarily unavailable (os error 11)");
    let result = crate::diag::record_writer_spawn_outcome::<()>(&slot, Err(err));
    let msg = result.expect_err("a failed spawn must be reported to the caller");
    assert!(
        msg.contains("Resource temporarily unavailable"),
        "the OS reason must survive into the recorded message so the \
         operator can tell an fd/thread exhaustion apart from anything \
         else, got {msg}"
    );
    assert_eq!(
        slot.get(),
        Some(&msg),
        "the reason must ALSO be latched in the slot - `async_writer_result` \
         reads it long after the spawn attempt, from a different thread"
    );
}

#[test]
fn record_writer_spawn_outcome_leaves_the_slot_clear_on_success() {
    let slot: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let result = crate::diag::record_writer_spawn_outcome(&slot, Ok(()));
    assert!(result.is_ok(), "a healthy spawn must report Ok");
    assert_eq!(
        slot.get(),
        None,
        "the absence of a recorded message IS 'the writer is running'; \
         a spurious entry would fail every hotkey install"
    );
}

#[test]
fn writer_spawn_failure_message_names_the_consequence() {
    let err = std::io::Error::other("no threads left");
    let msg = crate::diag::writer_spawn_failure_message(&err);
    assert!(
        msg.starts_with("[diag-async]"),
        "the marker must share the queue's log prefix so one grep finds \
         both the drop marker and the dead-writer line, got {msg}"
    );
    assert!(
        msg.contains("no threads left"),
        "the underlying io::Error must be quoted verbatim, got {msg}"
    );
    assert!(
        msg.is_ascii(),
        "diagnostic strings reach stderr under cmd.exe on a legacy code \
         page; non-ASCII renders as mojibake, got {msg}"
    );
}

// -----------------------------------------------------------------------
// Localized OS errors must not smuggle non-ASCII (or a newline) into a
// console line.
//
// Un-fixed behaviour: `writer_spawn_failure_message` interpolated `{err}`
// raw. `console_ascii_tests` scans source LITERALS, so it proves our
// prose is ASCII and is blind to what `{err}` expands to at runtime — and
// a `thread::Builder::spawn` error is OS-derived, rendered by
// `FormatMessageW` in the SYSTEM LOCALE. On a Danish/German/Japanese/
// Russian Windows the one line explaining why the diagnostic pipeline is
// dead becomes mojibake on a legacy-code-page cmd.exe. `Error::other`
// with an ASCII literal cannot reach that case, so these drive
// `ascii_escaped` directly with the shapes a localized OS message
// actually has.
// -----------------------------------------------------------------------

#[test]
fn ascii_escaped_passes_printable_ascii_through_untouched() {
    let plain = "Resource temporarily unavailable (os error 11)";
    assert_eq!(
        crate::diag::ascii_escaped(plain),
        plain,
        "the overwhelmingly common English case must not be disfigured - \
         an escape scheme that mangles the readable path would be reverted"
    );
}

#[test]
fn ascii_escaped_escapes_a_localized_os_error() {
    // Representative of what FormatMessageW returns for a thread-creation
    // failure on a non-English Windows: Danish, German and Russian.
    for localized in [
        "Der er ikke nok hukommelse til r\u{e5}dighed",
        "Nicht gen\u{fc}gend Speicher verf\u{fc}gbar",
        "\u{41d}\u{435}\u{434}\u{43e}\u{441}\u{442}\u{430}\u{442}\u{43e}\u{447}\u{43d}\u{43e} \u{43f}\u{430}\u{43c}\u{44f}\u{442}\u{438}",
    ] {
        let escaped = crate::diag::ascii_escaped(localized);
        assert!(
            escaped.is_ascii(),
            "a localized OS error must be reduced to ASCII before it reaches \
             a legacy-code-page console, got {escaped}"
        );
        assert!(
            escaped.contains("\\u{"),
            "the non-ASCII scalars must be escaped losslessly rather than \
             dropped, so a support thread can still recover the original \
             text from the log, got {escaped}"
        );
    }
}

#[test]
fn ascii_escaped_escapes_control_characters_so_a_record_stays_one_line() {
    // A newline inside an OS error would split one record into two and
    // break the one-line-per-record grep contract the tee file is read
    // with — a worse failure than mojibake, because the second half looks
    // like an unprefixed stray line.
    let escaped = crate::diag::ascii_escaped("first line\nsecond\ttab\r\n");
    assert!(
        !escaped.contains('\n') && !escaped.contains('\r') && !escaped.contains('\t'),
        "control characters must be escaped so one diagnostic record stays \
         on one line, got {escaped}"
    );
    assert!(escaped.is_ascii(), "result must be ASCII, got {escaped}");
    assert!(
        escaped.contains("\\u{a}"),
        "the newline must survive as a visible escape rather than be \
         dropped, got {escaped}"
    );
}

#[test]
fn writer_spawn_failure_message_sanitizes_a_localized_os_error() {
    // The production case the `Error::other`-with-an-ASCII-literal test
    // above cannot reach: an OS-derived error whose text is in the
    // system locale.
    let err = std::io::Error::other("Der er ikke nok hukommelse til r\u{e5}dighed");
    let msg = crate::diag::writer_spawn_failure_message(&err);
    assert!(
        msg.is_ascii(),
        "the whole line reaches stderr via write_line; a localized OS error \
         must not make it non-ASCII, got {msg}"
    );
    assert!(
        msg.contains("Der er ikke nok hukommelse til r\\u{e5}dighed"),
        "the localized reason must still be recoverable from the log, got {msg}"
    );
    assert!(
        msg.contains("cannot be written for the rest of this process"),
        "sanitizing must not cost the consequence half of the message, got {msg}"
    );
}

/// The healthy process-wide pipeline must report a live writer. Names
/// `async_writer_result` so the accessor the rdev listener depends on
/// has a test exercising it, and pins that the new check does not
/// spuriously fail an ordinary install.
#[test]
fn async_writer_result_is_ok_on_a_healthy_process() {
    let _guard = diag_test_lock();
    assert_eq!(
        crate::diag::async_writer_result(),
        Ok(()),
        "the real writer thread spawns fine in CI; an Err here would fail \
         every rdev hotkey install with SpawnError::WriterStartup"
    );
    assert!(
        crate::diag::async_writer_installed(),
        "async_writer_result must install the writer as a side effect, \
         exactly as ensure_async_writer does"
    );
}

/// Structural companion: the runtime tests above drive the recording
/// half; this one pins that PRODUCTION routes its spawn through it. The
/// un-fixed code was literally `let _ = thread::Builder::new()...`, so
/// re-introducing that swallow is the regression to catch.
#[test]
fn production_writer_spawn_failure_is_not_swallowed() {
    let install = scan_fn_body("src/rust/diag/startup.rs", "pub fn ensure_async_writer() {");
    assert!(
        install.code.contains("record_writer_spawn_outcome"),
        "ensure_async_writer must record the writer thread's spawn outcome \
         so `async_writer_result` can report a dead pipeline. Offending \
         function body:\n{}",
        install.raw
    );
    assert!(
        !install.code.contains("let _ = thread::Builder"),
        "ensure_async_writer must NOT discard the spawn Err - that is the \
         bug: the sender is installed regardless, so every callback-path \
         diagnostic is shed with no line anywhere saying why. Offending \
         function body:\n{}",
        install.raw
    );
}
