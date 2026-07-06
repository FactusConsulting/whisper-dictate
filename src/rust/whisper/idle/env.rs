//! Env-var parsing for the idle-unload timer.
//!
//! Split out from the wrapper so the parse rules (and their error messages)
//! can be exercised in pure unit tests without touching the model lifecycle.

use std::time::Duration;

use anyhow::{anyhow, Result};

/// Environment variable that controls the idle-unload timer.
///
/// Value semantics (parsed by [`parse_idle_timeout_from_env`]):
/// - **Unset or empty** → `None` (never unload — historical behaviour).
/// - **`"0"`** → `None` (explicit opt-out, easier to wire from a UI dropdown
///   whose "Never" option emits `0` rather than removing the variable).
/// - **Positive integer** → `Some(Duration::from_secs(n))`.
///
/// Anything else (negative, non-numeric, decimal, non-UTF-8) is a hard
/// error rather than a silent fallback — the user has expressed an intent,
/// and silently reinterpreting it as "never" would mask configuration bugs
/// in the wrapper that sets the variable.
pub const IDLE_UNLOAD_ENV: &str = "VOICEPI_WHISPER_IDLE_UNLOAD_S";

/// Read [`IDLE_UNLOAD_ENV`] and parse it into an optional idle window.
///
/// See the constant's docs for the value grammar. The three error modes
/// from [`std::env::var`] are handled distinctly:
///
/// - `NotPresent` → `Ok(None)` (no override, fall through to "never").
/// - `NotUnicode` → `Err` (an explicitly set value that we cannot decode is
///   a configuration bug worth surfacing loudly — on Unix this can happen
///   if a wrapper passes raw bytes through `OsString`; matching the
///   non-numeric path's behaviour rather than silently disabling unload).
/// - `Ok(raw)` → delegate to [`parse_idle_timeout_str`].
pub fn parse_idle_timeout_from_env() -> Result<Option<Duration>> {
    match std::env::var(IDLE_UNLOAD_ENV) {
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(raw)) => Err(anyhow!(
            "{IDLE_UNLOAD_ENV}={raw:?} is not valid UTF-8; \
             unset the variable or set it to a non-negative integer \
             (0 = never unload)"
        )),
        Ok(raw) => parse_idle_timeout_str(&raw),
    }
}

/// Pure helper for the env-var parser, split out for unit testing without
/// having to mutate the process environment.
pub(super) fn parse_idle_timeout_str(raw: &str) -> Result<Option<Duration>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let secs: u64 = trimmed.parse().map_err(|_| {
        anyhow!(
            "{IDLE_UNLOAD_ENV}={raw:?} is not a non-negative integer; \
             use 0 for 'never unload' or a positive seconds count"
        )
    })?;
    if secs == 0 {
        Ok(None)
    } else {
        Ok(Some(Duration::from_secs(secs)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};

    #[test]
    fn parse_env_unset_means_never() {
        assert_eq!(parse_idle_timeout_str("").unwrap(), None);
        assert_eq!(parse_idle_timeout_str("   ").unwrap(), None);
    }

    #[test]
    fn parse_env_zero_means_never() {
        assert_eq!(parse_idle_timeout_str("0").unwrap(), None);
        assert_eq!(parse_idle_timeout_str("  0 ").unwrap(), None);
    }

    #[test]
    fn parse_env_positive_returns_duration() {
        assert_eq!(
            parse_idle_timeout_str("30").unwrap(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_idle_timeout_str("3600").unwrap(),
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn parse_env_rejects_negative() {
        let err = parse_idle_timeout_str("-5").unwrap_err();
        assert!(
            err.to_string().contains("non-negative"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_env_rejects_non_numeric() {
        let err = parse_idle_timeout_str("forever").unwrap_err();
        assert!(
            err.to_string().contains("non-negative"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_env_rejects_decimal() {
        // We accept seconds-only (matching the UI dropdown). Decimal is
        // rejected with the same error rather than silently truncated.
        let err = parse_idle_timeout_str("1.5").unwrap_err();
        assert!(
            err.to_string().contains("non-negative"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_env_from_process_unset() {
        // RAII guard restores the original IDLE_UNLOAD_ENV value (Some/None)
        // even if the assertion panics — see Codex P2 #415 pattern.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvVarGuard::remove(IDLE_UNLOAD_ENV);

        assert_eq!(parse_idle_timeout_from_env().unwrap(), None);
    }

    #[test]
    fn parse_env_from_process_set() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvVarGuard::set(IDLE_UNLOAD_ENV, "42");

        assert_eq!(
            parse_idle_timeout_from_env().unwrap(),
            Some(Duration::from_secs(42))
        );
    }

    /// On Unix, an explicitly set non-UTF-8 value reaches us as
    /// `VarError::NotUnicode`. That is a configured-but-undecodable value:
    /// matching the documented hard-error path (rather than silently
    /// disabling unload like an unset variable) protects against
    /// wrapper/encoding bugs that would otherwise be invisible. Windows
    /// can't easily reproduce this from a Rust test without ffi tricks, so
    /// the assertion is unix-only — the production code path is portable.
    #[test]
    #[cfg(unix)]
    fn parse_env_from_process_rejects_non_unicode() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Lone 0xFF bytes are invalid UTF-8 in any normalization.
        let bad = OsString::from_vec(vec![0xFF, 0xFE, 0x80]);
        let _env = EnvVarGuard::set(IDLE_UNLOAD_ENV, &bad);

        let err = parse_idle_timeout_from_env().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("UTF-8"),
            "expected UTF-8 rejection message, got: {msg}"
        );
    }
}
