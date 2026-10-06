//! Redacted, sampled off-callback diagnostics.

use super::keys::{is_rdev_supported_name, key_to_name};
use super::{RAW_EVENT_INITIAL_TRACE, RAW_EVENT_TRACE_EVERY};

/// Rate-limit decision for the per-event trace line. Pure so it can be
/// unit-tested without spawning any threads — the runtime just calls it
/// with the current event counter (1-indexed).
///
/// Returns true for events `1..=RAW_EVENT_INITIAL_TRACE`, then for every
/// `RAW_EVENT_TRACE_EVERY`-th event thereafter. Zero is treated as "no
/// index" and always returns false; callers must pass at least 1.
pub(crate) fn should_log_raw_event(n: u64) -> bool {
    if n == 0 {
        return false;
    }
    if n <= RAW_EVENT_INITIAL_TRACE {
        return true;
    }
    n.is_multiple_of(RAW_EVENT_TRACE_EVERY)
}

// The off-callback queue infrastructure moved to `crate::diag` (see
// `enqueue_async` / `ensure_async_writer` there).
// The tracker's `[chord]` debug trace ALSO
// runs on the LL-hook callback thread and needs the same protection,
// so the queue is now shared across every callback-path call site.
// Historical name preserved as a re-export so `rdev_driver_tests`
// keeps compiling and any out-of-tree caller continues to work. The
// `allow(unused_imports)` is because production code inside this
// module no longer references the constant directly — only the test
// module (which lives in a companion `_tests.rs` compiled under
// `cfg(test)`) does.
#[allow(unused_imports)]
pub(crate) use crate::diag::ASYNC_QUEUE_CAPACITY as CALLBACK_TRACE_QUEUE_CAPACITY;

/// Historical wrapper — forwards to the shared [`crate::diag::enqueue_async`]
/// so an rdev-callback caller doesn't need to be updated to the new
/// name. Kept `pub(crate)` for the same reason: rdev_driver_tests and
/// the callback closure below still use this name.
pub(crate) fn enqueue_callback_trace(message: String) {
    crate::diag::enqueue_async(message);
}

/// Test-only alias for [`crate::diag::ensure_async_writer`] so
/// `rdev_driver_tests.rs` can drive the queue without going through a
/// full `spawn` (which would require the OS listener to install).
#[cfg(test)]
pub(crate) fn ensure_callback_trace_writer_for_tests() {
    crate::diag::ensure_async_writer();
}

/// Redact a raw-event name for the diagnostic-log trace line so ordinary
/// desktop typing (letters, digits, punctuation) never lands in
/// `gui-diagnostic.log`. Delegates to the shared
/// [`crate::hotkey::modifier_match::redact_key_name_for_diag`] so the
/// rdev pre-filter line, the tracker's `[chord]` line, and any future
/// hotkey trace all use the same PTT-eligibility predicate — otherwise
/// one surface redacts a name while another logs it raw.
pub(crate) fn redact_raw_event_name(name: &str) -> &str {
    crate::hotkey::modifier_match::redact_key_name_for_diag(name)
}

/// Redact an `rdev::EventType` for the debug-level pre-filter trace so
/// the `[rdev/callback] raw=…` line does not leak key identity for
/// ordinary desktop typing when `VOICEPI_LOG=debug`/`trace` is on.
///
/// The plain `{:?}` format of `rdev::EventType` prints the raw `Key`
/// variant — `KeyPress(KeyA)`, `KeyPress(Num5)`, `KeyPress(Semicolon)`
/// — for every desktop-wide keydown/keyup. That's exactly the identity
/// the sampled `[hotkey/rdev] raw event #n` line redacts a few lines
/// below; leaving this line unredacted defeats the redaction because
/// **every** event lands here (unsampled), so passwords, tokens and
/// URLs typed anywhere on the desktop can be reconstructed from a
/// debug/trace log window.
///
/// The rule mirrors [`redact_raw_event_name`]: for a key event, if the
/// resolved name is PTT-eligible ([`is_rdev_supported_name`]) keep it
/// (that's the debug-diagnostic signal we care about — an F9 rdev sees
/// but `key_to_name` discards vs. one it maps cleanly); otherwise emit
/// `KeyPress(<redacted>)` / `KeyRelease(<redacted>)`. Non-key events
/// (mouse move, wheel, button) carry no keyboard PII and pass through
/// as their `{:?}` form so mouse-hook interaction stays diagnosable.
pub(crate) fn redact_event_type_for_debug(event_type: &rdev::EventType) -> String {
    match event_type {
        rdev::EventType::KeyPress(k) => match key_to_name(*k) {
            Some(name) if is_rdev_supported_name(&name) => format!("KeyPress({name})"),
            _ => "KeyPress(<redacted>)".to_owned(),
        },
        rdev::EventType::KeyRelease(k) => match key_to_name(*k) {
            Some(name) if is_rdev_supported_name(&name) => format!("KeyRelease({name})"),
            _ => "KeyRelease(<redacted>)".to_owned(),
        },
        other => format!("{other:?}"),
    }
}
