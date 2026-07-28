//! Companion tests for [`crate::hotkey::manager::tracker`] that need
//! the process-wide diagnostic sink (`crate::diag`) — kept out of the
//! inline `#[cfg(test)] mod tests` inside `tracker.rs` so that file
//! stays under the 500-LOC modularity rule (`AGENTS.md`) and so the
//! env-var-mutation lock and diag file-writer lock stay in one
//! testable file.
//!
//! The pure-state-machine tests (bare-modifier rules, foreign-key
//! self-heal, side-specific matching) remain inline in `tracker.rs`
//! itself — they don't touch the diag sink, so no serial-mutex dance
//! is required. Only the log-shape / redaction tests live here.

#![cfg(test)]

use std::io::Read;
use std::path::Path;
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use crate::diag::{
    init_from_env, install_gui_diagnostic_log, reset_level_for_tests, LogLevel, LOG_ENV_VAR,
};
use crate::diag_test_lock::DIAG_WRITER_LOCK;
use crate::hotkey::manager::tracker::{
    format_chord_diag_line, KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput,
};

/// Poll `path` up to `timeout` for `needle`, returning the file
/// contents once the substring appears or the timeout elapses.
/// Codex P2 #675 PRRT_kwDOSfNjQs6UbAiI: the chord line now goes
/// through the async diagnostic writer (previously it landed in the
/// tee file synchronously), so a follow-up file read races the
/// writer thread and needs a bounded poll.
fn wait_for_substring(path: &Path, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut contents = String::new();
    loop {
        contents.clear();
        let _ = std::fs::File::open(path).and_then(|mut f| f.read_to_string(&mut contents));
        if contents.contains(needle) || Instant::now() >= deadline {
            return contents;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Serialise diag-mutation tests so parallel runs don't race the
/// process-wide writer slot. Uses the shared crate-wide
/// [`DIAG_WRITER_LOCK`] so tests in this module cannot race the
/// tests in `diag_tests.rs` (or any future module that installs a
/// diagnostic writer). Codex P2 #665 discussion
/// PRRT_kwDOSfNjQs6UYDJB.
fn diag_test_lock() -> MutexGuard<'static, ()> {
    DIAG_WRITER_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn press(name: &str) -> RawKeyEvent {
    RawKeyEvent {
        name: name.to_owned(),
        kind: RawKeyKind::Press,
        at: Instant::now(),
    }
}

// -----------------------------------------------------------------------
// Codex P1 #665 review (thread PRRT_kwDOSfNjQs6UXh5C) — the tracker's
// `[chord]` line MUST NOT log a raw key identity for non-PTT keys.
//
// Failure mode this test would exhibit against the un-fixed code
// (the version that logged `event.name` and `held` verbatim):
//   * The temp diag file would contain the literal `KeyA` /
//     `__rdev_KeyA` sequence — the exact identity a
//     `VOICEPI_LOG=debug`/`trace` window is expected NOT to leak.
//   * The `assert!(!contents.contains("__rdev_KeyA"))` line below
//     would panic and the test fails.
//
// The fix routes both `event.name` and every entry of the pre-event
// `held` snapshot through
// `crate::hotkey::modifier_match::redact_key_name_for_diag`, so the
// only identity in the line is the `<redacted>` sentinel.
// -----------------------------------------------------------------------

#[test]
fn chord_trace_redacts_ordinary_typing_at_debug_level() {
    let _diag_lock = diag_test_lock();
    let _env_guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let prev_env = std::env::var(LOG_ENV_VAR).ok();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chord.log");
    install_gui_diagnostic_log(&path).expect("install diag");

    std::env::set_var(LOG_ENV_VAR, "debug");
    reset_level_for_tests();
    assert_eq!(init_from_env(), LogLevel::Debug);

    let mut tracker = KeyTracker::new(vec!["ctrl_l".to_owned(), "f9".to_owned()]);
    // Push a press for a synthetic name — the exact shape
    // `raw_from_rdev` produces for any unmapped desktop key
    // (letters/digits/punctuation the user types into other apps).
    let _ = tracker.handle(&press("__rdev_KeyA"));

    // Chord line now travels through the async diag writer — poll the
    // tee file until the [chord] marker lands or the timeout expires
    // (Codex P2 #675 PRRT_kwDOSfNjQs6UbAiI). 500 ms is generous for a
    // single-record drain on CI.
    let contents = wait_for_substring(&path, "[chord]", Duration::from_millis(500));

    assert!(
        contents.contains("[chord]"),
        "debug-level KeyTracker::handle must emit a `[chord]` line so the \
         redaction gate actually fires; got: {contents:?}"
    );
    assert!(
        contents.contains("<redacted>"),
        "the redacted sentinel must appear in place of the synthetic key \
         name so ordinary typing does not leak identity; got: {contents:?}"
    );
    assert!(
        !contents.contains("__rdev_KeyA"),
        "the raw synthetic name (`__rdev_KeyA`, produced by `raw_from_rdev` \
         for unmapped keys) MUST NOT reach the diagnostic log — that's the \
         Codex P1 #665 regression this test locks. got: {contents:?}"
    );
    // Sanity: `KeyA` alone would also be a leak (the OS name for
    // some keyboard layouts / `enigo::Key` variants). Pin it too so
    // a redactor that stripped only the `__rdev_` prefix cannot
    // sneak through.
    assert!(
        !contents.contains("KeyA"),
        "no substring of the raw variant name (`KeyA`) may appear either; \
         got: {contents:?}"
    );

    match prev_env {
        Some(v) => std::env::set_var(LOG_ENV_VAR, v),
        None => std::env::remove_var(LOG_ENV_VAR),
    }
    reset_level_for_tests();
}

#[test]
fn chord_trace_preserves_ptt_eligible_names_at_debug_level() {
    // Counterpart to the redaction test: the whole diagnostic value
    // of the `[chord]` line is spotting cases like "the tracker saw
    // `ctrl` when the binding wanted `ctrl_l`" — so PTT-eligible key
    // names MUST survive the redactor. A regression that redacted
    // EVERYTHING (over-broad predicate) would fail this test.
    let _diag_lock = diag_test_lock();
    let _env_guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let prev_env = std::env::var(LOG_ENV_VAR).ok();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chord-ptt.log");
    install_gui_diagnostic_log(&path).expect("install diag");

    std::env::set_var(LOG_ENV_VAR, "debug");
    reset_level_for_tests();
    assert_eq!(init_from_env(), LogLevel::Debug);

    let mut tracker = KeyTracker::new(vec!["ctrl_l".to_owned(), "f9".to_owned()]);
    let _ = tracker.handle(&press("ctrl_l"));
    let _ = tracker.handle(&press("f9"));

    // The [chord] line goes through the async diag writer, so poll
    // for the second event (`f9` press) to guarantee both records
    // reached the tee file before we read it.
    let contents = wait_for_substring(&path, "event=f9", Duration::from_millis(500));

    assert!(
        contents.contains("event=ctrl_l"),
        "PTT-eligible name `ctrl_l` must survive the redactor verbatim \
         (that's the whole point of the trace); got: {contents:?}"
    );
    assert!(
        contents.contains("event=f9"),
        "PTT-eligible name `f9` must survive the redactor verbatim; \
         got: {contents:?}"
    );

    match prev_env {
        Some(v) => std::env::set_var(LOG_ENV_VAR, v),
        None => std::env::remove_var(LOG_ENV_VAR),
    }
    reset_level_for_tests();
}

// -----------------------------------------------------------------------
// Codex P2 #675 PRRT_kwDOSfNjQs6UbAiI — the `[chord]` line MUST be
// routed through the async diagnostic writer, not the synchronous
// `crate::diag::log!` path. Previously the tracker acquired the diag
// writer mutex + flushed the AppData tee file from inside
// `KeyTracker::handle`, which on Windows runs on the LL-hook callback
// thread; a slow write there can exceed Windows' ~300 ms callback
// budget and cause the OS to silently uninstall the PTT hook. The
// fix routes the line through the same `diag_async` queue the
// `[rdev/callback]` records already use.
//
// `format_chord_diag_line` is the pure formatter the routing helper
// calls — asserting on it directly lets us pin the exact grep-shape
// support runbooks depend on WITHOUT touching the diag sink.
// -----------------------------------------------------------------------

#[test]
fn format_chord_diag_line_produces_grep_friendly_shape() {
    let line = format_chord_diag_line(
        "ctrl_l",
        RawKeyKind::Press,
        &["ctrl_l".to_owned()],
        &["ctrl_l".to_owned(), "f9".to_owned()],
        Some(TrackerOutput::ChordPress),
    );
    assert!(
        line.starts_with("[chord] "),
        "chord line must begin with `[chord] ` so support runbook greps \
         keep working; got: {line:?}"
    );
    assert!(line.contains("event=ctrl_l/Press"));
    assert!(line.contains("chord_target=[\"ctrl_l\", \"f9\"]"));
    assert!(line.contains("match=Some(ChordPress)"));
}

#[test]
fn format_chord_diag_line_redacts_non_ptt_event_names() {
    // Non-PTT event names (letters/digits/punctuation typed while
    // the LL hook was alive) MUST be replaced with `<redacted>` in
    // the event= field — the Codex P1 #665 redaction contract still
    // applies now that the line goes via the async writer.
    let line = format_chord_diag_line(
        "__rdev_KeyA",
        RawKeyKind::Press,
        &["<redacted>".to_owned()],
        &["ctrl_l".to_owned()],
        None,
    );
    assert!(
        line.contains("event=<redacted>/Press"),
        "non-PTT event name must be redacted in the formatted chord line; \
         got: {line:?}"
    );
    assert!(
        !line.contains("__rdev_KeyA"),
        "the pre-redaction identity MUST NOT survive the formatter; \
         got: {line:?}"
    );
}
