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
//! Resolution order (first hit wins):
//!
//! | Candidate | Why |
//! |---|---|
//! | `$VOICEPI_PTT_LOCK_DIR` | Test seam + operator escape hatch. |
//! | `$XDG_RUNTIME_DIR` | Linux per-user tmpfs (`/run/user/<uid>`); cleared on logout, so it cannot accumulate. |
//! | `%LOCALAPPDATA%\WhisperDictate` | Windows per-user, matches the diagnostic log's home (`diag::default_gui_diagnostic_path`). |
//! | `std::env::temp_dir()` | Last resort; the user tag in the file name carries the per-user separation here. |

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

/// Directory the lock pair lives in for this process. See the module docs
/// for the resolution order.
pub fn lock_dir() -> PathBuf {
    resolve_lock_dir(
        std::env::var_os(LOCK_DIR_ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("LOCALAPPDATA"),
        std::env::temp_dir(),
    )
}

/// Pure resolver behind [`lock_dir`]. Every candidate is injected so the
/// table can be unit-tested without mutating process env (which races the
/// rest of the test binary).
///
/// Empty / whitespace-only values are treated as unset: `XDG_RUNTIME_DIR=`
/// is a leftover in a login script, not a directory choice.
pub fn resolve_lock_dir(
    override_dir: Option<OsString>,
    xdg_runtime_dir: Option<OsString>,
    local_app_data: Option<OsString>,
    temp_dir: PathBuf,
) -> PathBuf {
    if let Some(dir) = non_empty(override_dir) {
        return dir;
    }
    if let Some(dir) = non_empty(xdg_runtime_dir) {
        return dir;
    }
    if let Some(dir) = non_empty(local_app_data) {
        return dir.join("WhisperDictate");
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
