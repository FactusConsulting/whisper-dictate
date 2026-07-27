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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Set a fake HOME so the assertion is deterministic on any machine.
    /// Serialised through the crate ENV_LOCK because `set_var` /
    /// `remove_var` are process-global.
    #[test]
    fn expands_leading_tilde_from_home_env() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let saved_home = std::env::var_os("HOME");
        let saved_userprofile = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", home);
        std::env::set_var("USERPROFILE", home);

        assert_eq!(expand_user("~"), home.to_path_buf());
        assert_eq!(expand_user("~/history.jsonl"), home.join("history.jsonl"));
        // Windows-flavoured separator inside a `~\path` -- must also be stripped.
        assert_eq!(expand_user("~\\history.jsonl"), home.join("history.jsonl"));
        // Absolute paths pass through untouched.
        assert_eq!(
            expand_user("/tmp/history.jsonl"),
            PathBuf::from("/tmp/history.jsonl")
        );

        // Restore env so sibling tests are unaffected.
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
    }
}
