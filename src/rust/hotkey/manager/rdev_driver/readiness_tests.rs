//! readiness regression tests for the rdev driver.

use super::*;

// -----------------------------------------------------------------------
// Writer-spawn failure is surfaced (was: silently swallowed).
//
// `ensure_async_writer` used to discard the `thread::Builder::spawn`
// Err. The sender was installed regardless, so `async_writer_installed()`
// reported true, `enqueue_async` kept filling a queue nobody read, and
// once the 256 slots were gone every callback-path diagnostic - the rdev
// boundary trace, the tracker `[chord]` trace, the Windows raw-hook
// trace - vanished with nothing anywhere saying why. In
// `gui-diagnostic.log` that process is indistinguishable from a healthy
// quiet one, which makes any subsequent PTT wedge report undiagnosable.
//
// The fix: the listener primes the writer BEFORE announcing readiness
// and reports `ListenerSignal::WriterFailed`, which the spawn side maps
// to `SpawnError::WriterStartup` so `install_hotkey()` fails loudly and
// the supervisor keeps the alternate listener wired.
// -----------------------------------------------------------------------

#[test]
fn listener_handshake_reports_a_dead_writer_instead_of_announcing_ready() {
    let (tx, rx) = std::sync::mpsc::channel::<ListenerSignal>();
    let outcome = listener_readiness_handshake(&tx, Err("spawn refused".to_owned()), true);
    assert_eq!(
        outcome,
        ListenerStart::AbortWriterFailed,
        "a failed diagnostic writer must abort the listener BEFORE it          installs the OS hook - proceeding would give the caller an Ok          spawn on a driver whose every callback diagnostic is shed"
    );
    match rx.try_recv() {
        Ok(ListenerSignal::WriterFailed(msg)) => assert_eq!(
            msg, "spawn refused",
            "the writer's own failure reason must reach the spawn side              verbatim so the operator sees WHY the pipeline is dead"
        ),
        other => panic!(
            "expected WriterFailed carrying the writer's reason, got {other:?} -              a `Started` here is the un-fixed behaviour: spawn returns Ok              and the process runs blind"
        ),
    }
}

#[test]
fn listener_handshake_announces_started_when_the_writer_is_healthy() {
    let (tx, rx) = std::sync::mpsc::channel::<ListenerSignal>();
    let outcome = listener_readiness_handshake(&tx, Ok(()), true);
    assert_eq!(
        outcome,
        ListenerStart::Proceed,
        "a healthy writer must not change the pre-existing startup path"
    );
    assert!(
        matches!(rx.try_recv(), Ok(ListenerSignal::Started)),
        "the healthy path must still announce Started, or `spawn` would          report ListenerHung on every install"
    );
}

/// A dead diagnostic writer must not disable the OS hook when logging is off.
///
/// At `VOICEPI_LOG` = `off` / `error` / `warn`, every enqueue site on the
/// rdev callback path is already switched off by its own
/// `info_enabled` / `debug_enabled` gate (that is what
/// `crate::diag::callback_diagnostics_enabled` means). A writer that
/// failed to spawn is therefore a writer nobody would have used, and
/// losing it cannot make the requested log incomplete. Aborting anyway
/// turns a working Rust-hotkey install into a downgrade - and at
/// `off` the line explaining the downgrade is suppressed too, so the user
/// gets a silently worse hotkey path in exchange for a diagnostic they
/// switched off.
///
/// Un-fixed behaviour (an unconditional abort): the outcome is
/// `AbortWriterFailed` and a `WriterFailed` signal goes to the spawn
/// side, so `install_hotkey()` fails and the supervisor falls back.
#[test]
fn listener_handshake_keeps_the_hook_when_callback_tracing_is_disabled() {
    let (tx, rx) = std::sync::mpsc::channel::<ListenerSignal>();
    let outcome = listener_readiness_handshake(&tx, Err("spawn refused".to_owned()), false);
    assert_eq!(
        outcome,
        ListenerStart::Proceed,
        "with callback-path diagnostics disabled the OS hook is worth \
         strictly more than the writer that would have had nothing to \
         write: aborting here downgrades a working Rust hotkey install to \
         a downgrade over a log nobody asked for"
    );
    assert!(
        matches!(rx.try_recv(), Ok(ListenerSignal::Started)),
        "the listener must still announce Started, or `spawn` reports \
         ListenerHung and the install fails for a different reason"
    );
}

/// The gate is about the CALLBACK path only - a healthy writer at a
/// silent log level must still take the ordinary Started path, so the
/// fix above cannot be read as "ignore the writer".
#[test]
fn listener_handshake_is_unchanged_by_the_gate_when_the_writer_is_healthy() {
    for callback_diagnostics_enabled in [false, true] {
        let (tx, rx) = std::sync::mpsc::channel::<ListenerSignal>();
        let outcome = listener_readiness_handshake(&tx, Ok(()), callback_diagnostics_enabled);
        assert_eq!(
            outcome,
            ListenerStart::Proceed,
            "a healthy writer must Proceed at every log level \
             (callback_diagnostics_enabled={callback_diagnostics_enabled})"
        );
        assert!(
            matches!(rx.try_recv(), Ok(ListenerSignal::Started)),
            "Started must go out at every log level \
             (callback_diagnostics_enabled={callback_diagnostics_enabled})"
        );
    }
}

/// The production gate must be the level union, not a hand-picked
/// subset: `callback_diagnostics_enabled` has to be true exactly when at
/// least one callback-path enqueue site can fire. The most permissive of
/// those sites is info-gated (the rdev per-event sample), so the union IS
/// `info_enabled`. A regression that gated on `debug_enabled` would make
/// a genuinely dead writer non-fatal at `info` - where the `raw event #n`
/// trace the operator is reading DOES get shed.
#[test]
fn callback_diagnostics_gate_is_the_union_of_the_callback_site_gates() {
    let _diag_lock = diag_test_lock();
    for (level, expected) in [
        (crate::diag::LogLevel::Off, false),
        (crate::diag::LogLevel::Error, false),
        (crate::diag::LogLevel::Warn, false),
        (crate::diag::LogLevel::Info, true),
        (crate::diag::LogLevel::Debug, true),
        (crate::diag::LogLevel::Trace, true),
    ] {
        crate::diag::set_level_for_tests(level);
        assert_eq!(
            crate::diag::callback_diagnostics_enabled(),
            expected,
            "callback diagnostics at {} must be {expected}",
            level.as_str()
        );
        assert!(
            !(crate::diag::debug_enabled() || crate::diag::trace_enabled())
                || crate::diag::callback_diagnostics_enabled(),
            "the gate must cover every callback-path site: {} enables a \
             debug/trace enqueue site the gate says is off",
            level.as_str()
        );
    }
    crate::diag::reset_level_for_tests();
}

#[test]
fn await_listener_ready_maps_writer_failure_to_writer_startup_error() {
    let (tx, rx) = std::sync::mpsc::channel::<ListenerSignal>();
    tx.send(ListenerSignal::WriterFailed("spawn refused".to_owned()))
        .expect("send");
    match await_listener_ready(&rx) {
        Err(SpawnError::WriterStartup(msg)) => assert_eq!(
            msg, "spawn refused",
            "the reason must survive the mapping so `install_hotkey`'s error names the dead diagnostic pipeline"
        ),
        other => panic!(
            "a WriterFailed signal must become SpawnError::WriterStartup, got {other:?}; classifying it as ListenerStartup would misdirect the operator toward the OS hook"
        ),
    }
}

#[test]
fn await_listener_ready_still_maps_the_pre_existing_signals() {
    // WriterStartup must be an ADDITION, not a reclassification.
    let (tx, rx) = std::sync::mpsc::channel::<ListenerSignal>();
    tx.send(ListenerSignal::Failed("no X display".to_owned()))
        .expect("send");
    assert!(
        matches!(await_listener_ready(&rx), Err(SpawnError::ListenerStartup(msg)) if msg == "no X display"),
        "an rdev listen() failure must still surface as ListenerStartup"
    );

    // A dropped sender with nothing queued is the "thread never
    // scheduled" case.
    let (tx2, rx2) = std::sync::mpsc::channel::<ListenerSignal>();
    drop(tx2);
    assert!(
        matches!(await_listener_ready(&rx2), Err(SpawnError::ListenerHung)),
        "a listener that never signals must still surface as ListenerHung"
    );
}
