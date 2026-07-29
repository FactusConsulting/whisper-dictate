//! Where the PTT ownership lock lives.
//!
//! Two constraints shape the choice:
//!
//! 1. **Per user, not per machine.** On Linux `/tmp` is shared by every
//!    account on the box. A lock keyed on a fixed name there would let one
//!    user's running GUI refuse a DIFFERENT user's launch — a false
//!    refusal, which is the failure mode this whole guard must not
//!    introduce. So the preferred directory is the per-user runtime dir,
//!    and the file name carries a user tag ONLY where the directory
//!    itself is shared.
//!
//!    That "only" is load-bearing (Codex P2 #688). The tag comes from
//!    `USER` / `USERNAME`, and those can disagree for the *same* Windows
//!    account: an Explorer-launched GUI sees only `USERNAME`, while a CLI
//!    started from Git Bash or WSL inherits a different `USER`. Tagging
//!    inside an already-per-user directory would then yield two file
//!    names in one directory for one account — both acquisitions succeed
//!    and the guard protects nothing. Every Windows candidate is already
//!    per-user (`%LOCALAPPDATA%`, and `%LOCALAPPDATA%\Temp` behind
//!    `temp_dir()`), so Windows never tags at all.
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
//! | `std::env::temp_dir()` | all | Last resort. On Unix this is the one shared candidate, so the file name is user-tagged there and only there. |
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

/// A resolved lock directory plus whether the directory ITSELF already
/// separates users.
///
/// The flag decides the file name: a shared directory needs the account
/// tag, a per-user one must not have it (see the module docs for why
/// tagging a per-user directory is actively harmful on Windows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockDir {
    pub dir: PathBuf,
    /// True when the directory is per-user, so a fixed file name is
    /// already unambiguous.
    pub per_user: bool,
}

/// Directory the lock pair lives in for this process. See the module docs
/// for the resolution order.
pub fn lock_dir() -> LockDir {
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
) -> LockDir {
    if let Some(dir) = non_empty(override_dir) {
        // An explicit override is an isolation request in itself (tests,
        // and operators who deliberately want two independent instances).
        // Treating it as per-user keeps the file name fixed, so two
        // processes pointed at the same override always meet.
        return LockDir {
            dir,
            per_user: true,
        };
    }
    match platform {
        Platform::Unix => {
            if let Some(dir) = non_empty(xdg_runtime_dir) {
                // `/run/user/<uid>` is per-uid by construction.
                return LockDir {
                    dir,
                    per_user: true,
                };
            }
            // `/tmp` is the one genuinely shared candidate: tag it.
            LockDir {
                dir: temp_dir,
                per_user: false,
            }
        }
        Platform::Windows => {
            if let Some(dir) = non_empty(local_app_data) {
                return LockDir {
                    dir: dir.join("WhisperDictate"),
                    per_user: true,
                };
            }
            // `temp_dir()` on Windows is `%LOCALAPPDATA%\Temp` (or
            // `%USERPROFILE%\AppData\Local\Temp`) -- still per-user, and
            // tagging it would reintroduce the USER / USERNAME split.
            LockDir {
                dir: temp_dir,
                per_user: true,
            }
        }
    }
}

/// `Some(path)` when `value` is present and not blank.
fn non_empty(value: Option<OsString>) -> Option<PathBuf> {
    let value = value?;
    if value.to_string_lossy().trim().is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// File-name suffix that separates accounts sharing one directory.
///
/// Empty for a per-user directory — see [`resolve_name_suffix`] for why
/// that case must NOT be tagged. Otherwise `-<account>`, sanitised.
pub fn name_suffix(location: &LockDir) -> String {
    resolve_name_suffix(
        location.per_user,
        std::env::var_os("USER")
            .or_else(|| std::env::var_os("USERNAME"))
            .map(|v| v.to_string_lossy().into_owned()),
    )
}

/// Pure half of [`name_suffix`].
///
/// The `per_user` short-circuit is the fix for Codex P2 #688: `USER` and
/// `USERNAME` can disagree for one Windows account (Explorer sets only
/// `USERNAME`; Git Bash / WSL export a `USER` that may differ), so tagging
/// inside an already-per-user directory would give the SAME account two
/// file names — two successful acquisitions, and a guard that protects
/// nothing. The tag exists solely to separate different accounts sharing
/// `/tmp`, so it is applied solely there.
pub fn resolve_name_suffix(per_user: bool, account: Option<String>) -> String {
    if per_user {
        return String::new();
    }
    format!("-{}", resolve_user_tag(account))
}

/// Sanitised account name for the shared-directory suffix. Falls back to
/// `unknown`, which is still correct (everyone lacking both env vars
/// shares one lock) but is not a configuration we expect.
pub fn resolve_user_tag(raw: Option<String>) -> String {
    sanitize_token(raw.unwrap_or_default().trim())
}

/// `<dir>/whisper-dictate-ptt<suffix>.lock` — the file whose OS lock is
/// the ownership token. `suffix` comes from [`name_suffix`] and is empty
/// in a per-user directory.
pub fn lock_path_in(dir: &Path, suffix: &str) -> PathBuf {
    dir.join(format!("{BASE_NAME}{suffix}.{LOCK_EXT}"))
}

/// `<dir>/whisper-dictate-ptt<suffix>.owner` — the advisory holder record.
pub fn owner_path_in(dir: &Path, suffix: &str) -> PathBuf {
    dir.join(format!("{BASE_NAME}{suffix}.{OWNER_EXT}"))
}
