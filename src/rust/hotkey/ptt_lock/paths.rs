//! Where the PTT ownership lock lives.
//!
//! Two constraints shape the choice:
//!
//! 1. **Per user, not per machine.** On Linux `/tmp` is shared by every
//!    account on the box. A lock keyed on a fixed name there would let one
//!    user's running GUI refuse a DIFFERENT user's launch — a false
//!    refusal, which is the failure mode this whole guard must not
//!    introduce. So the preferred directory is the per-user runtime dir,
//!    and the file name carries a user tag as a second line of defence for
//!    the shared-temp fallback.
//!
//! 2. **Writable on a locked-down install.** The GUI can run from
//!    `C:\Program Files\`, so the lock must never land next to the
//!    executable. Every candidate below is a user-writable location.
//!
//! Resolution order (first hit wins), and it is **platform-gated**:
//!
//! | Candidate | Platform | Why |
//! |---|---|---|
//! | `$VOICEPI_PTT_LOCK_DIR` | all | Test seam + operator escape hatch. |
//! | `$XDG_RUNTIME_DIR` | Unix only | Per-user tmpfs (`/run/user/<uid>`); cleared on logout, so it cannot accumulate. |
//! | `%LOCALAPPDATA%\WhisperDictate` | Windows only | Per-user, matches the diagnostic log's home (`diag::default_gui_diagnostic_path`). |
//! | `std::env::temp_dir()` | all | Last resort; the user tag in the file name carries the per-user separation here. |
//!
//! The platform gate is load-bearing, not tidiness (Codex P2 #688).
//! `XDG_RUNTIME_DIR` is routinely exported on Windows by Git Bash, MSYS2
//! and WSL-adjacent shells. Honouring it there would mean a tray GUI
//! launched from Explorer and a CLI launched from such a shell resolve
//! DIFFERENT lock files — so both acquisitions succeed, and the guard
//! silently protects nothing in exactly the two-process scenario it
//! exists for. (If the POSIX-looking path is unusable the CLI takes the
//! fail-open path instead, with the same outcome.) One canonical
//! per-user location per platform is the whole point.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::record::sanitize_token;

/// Operator / test override for the lock directory.
pub const LOCK_DIR_ENV: &str = "VOICEPI_PTT_LOCK_DIR";

/// Base name shared by the lock file and its holder-record sibling.
const BASE_NAME: &str = "whisper-dictate-ptt";

/// Extension of the file whose OS lock IS the ownership token. Always
/// zero bytes: on Windows its byte range is mandatory-locked, so nothing
/// may be stored in it.
const LOCK_EXT: &str = "lock";

/// Extension of the unlocked advisory sibling holding the
/// [`super::record::HolderRecord`].
const OWNER_EXT: &str = "owner";

/// Which per-user directory convention applies. Passed explicitly into
/// [`resolve_lock_dir`] so the platform gate is a unit-testable argument
/// rather than a `#[cfg]` the test suite can only exercise on one OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// `XDG_RUNTIME_DIR` is the per-user runtime directory.
    Unix,
    /// `%LOCALAPPDATA%` is the per-user data directory; `XDG_RUNTIME_DIR`
    /// is ignored even when a shell exports it (see the module docs).
    Windows,
}

impl Platform {
    /// The convention for the platform this binary was built for.
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// Directory the lock pair lives in for this process. See the module docs
/// for the resolution order.
pub fn lock_dir() -> PathBuf {
    resolve_lock_dir(
        Platform::current(),
        std::env::var_os(LOCK_DIR_ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("LOCALAPPDATA"),
        std::env::temp_dir(),
    )
}

/// Pure resolver behind [`lock_dir`]. Every candidate — including the
/// platform — is injected so the whole table can be unit-tested on any
/// host, without mutating process env (which races the rest of the test
/// binary).
///
/// Empty / whitespace-only values are treated as unset: `XDG_RUNTIME_DIR=`
/// is a leftover in a login script, not a directory choice.
pub fn resolve_lock_dir(
    platform: Platform,
    override_dir: Option<OsString>,
    xdg_runtime_dir: Option<OsString>,
    local_app_data: Option<OsString>,
    temp_dir: PathBuf,
) -> PathBuf {
    if let Some(dir) = non_empty(override_dir) {
        return dir;
    }
    match platform {
        Platform::Unix => {
            if let Some(dir) = non_empty(xdg_runtime_dir) {
                return dir;
            }
        }
        Platform::Windows => {
            if let Some(dir) = non_empty(local_app_data) {
                return dir.join("WhisperDictate");
            }
        }
    }
    temp_dir
}

/// `Some(path)` when `value` is present and not blank.
fn non_empty(value: Option<OsString>) -> Option<PathBuf> {
    let value = value?;
    if value.to_string_lossy().trim().is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// Sanitised account name used to keep the lock per-user in a shared
/// temp directory. Falls back to `unknown`, which is still correct
/// (everyone lacking both env vars shares one lock) but is not a
/// configuration we expect on either supported platform.
pub fn user_tag() -> String {
    resolve_user_tag(
        std::env::var_os("USER")
            .or_else(|| std::env::var_os("USERNAME"))
            .map(|v| v.to_string_lossy().into_owned()),
    )
}

/// Pure half of [`user_tag`].
pub fn resolve_user_tag(raw: Option<String>) -> String {
    sanitize_token(raw.unwrap_or_default().trim())
}

/// `<dir>/whisper-dictate-ptt-<user>.lock` — the file whose OS lock is the
/// ownership token.
pub fn lock_path_in(dir: &Path, user: &str) -> PathBuf {
    dir.join(format!("{BASE_NAME}-{user}.{LOCK_EXT}"))
}

/// `<dir>/whisper-dictate-ptt-<user>.owner` — the advisory holder record.
pub fn owner_path_in(dir: &Path, user: &str) -> PathBuf {
    dir.join(format!("{BASE_NAME}-{user}.{OWNER_EXT}"))
}
