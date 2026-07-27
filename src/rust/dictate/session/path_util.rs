//! Path helpers shared across the session sinks.
//!
//! Extracted from `metrics_sink.rs` so `history_sink.rs` can honour the
//! same `~` expansion Python's `os.path.expanduser` does when the user
//! writes `~/.voicepi/history.jsonl` into `history_jsonl` (Codex P2
//! #620 history_sink.rs:107). Kept module-private (`mod path_util;`) so
//! only the session sinks reach it -- the sibling
//! `dictionary::store::expand_user` / `corpus::expand_tilde` copies exist
//! for the same reason in their own subsystems and stay independent.

use std::path::PathBuf;

/// Expand a leading `~` to the user's home directory, matching Python's
/// `os.path.expanduser`. Anything without a leading `~` is returned as-is.
/// A missing `HOME`/`USERPROFILE` falls through to `.` -- the same
/// last-resort the sibling `dictionary::store::expand_user` and
/// `corpus::expand_tilde` helpers pick.
pub(super) fn expand_user(raw: &str) -> PathBuf {
    if let Some(stripped) = raw.strip_prefix('~') {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let rest = stripped.trim_start_matches(['/', '\\']);
        if rest.is_empty() {
            return home;
        }
        return home.join(rest);
    }
    PathBuf::from(raw)
}

// Tests live in the sibling `path_util_tests.rs` so the
// regression-test discipline scanner (`test_regression_test_discipline.py`)
// picks them up as "the matching test file" for this module.
// Registered via `#[path]` on the `mod path_util_tests;` in `mod.rs`.
