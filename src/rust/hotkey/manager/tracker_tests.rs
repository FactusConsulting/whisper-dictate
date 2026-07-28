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
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use crate::diag::{
    init_from_env, install_gui_diagnostic_log, reset_level_for_tests, LogLevel, LOG_ENV_VAR,
};
use crate::hotkey::manager::tracker::{KeyTracker, RawKeyEvent, RawKeyKind};

/// Serialise diag-mutation tests so parallel runs don't race the
/// shared writer slot: two tests installing to different temp paths
/// simultaneously would each see the other's writes. Mirrors the
/// same lock in `diag_tests.rs`.
fn diag_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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

    let mut contents = String::new();
    std::fs::File::open(&path)
        .expect("open chord log")
        .read_to_string(&mut contents)
        .expect("read chord log");

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

    let mut contents = String::new();
    std::fs::File::open(&path)
        .expect("open chord log")
        .read_to_string(&mut contents)
        .expect("read chord log");

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
