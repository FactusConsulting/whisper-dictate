//! Serialize app-managed JSONL writers through a stable sibling lock.
//!
//! Lock the sidecar, not the data file: Windows file locks would otherwise
//! prevent reading/replacement. Never unlink a lock while other handles can
//! be waiting on it. External/non-cooperating writers are not covered.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) struct LockedFile {
    pub(crate) path: PathBuf,
    _lock: File,
}

/// Existing aliases use the canonical destination; missing files use their
/// canonical parent. Reject dangling aliases rather than lock a different path.
pub(crate) fn identity(path: &Path) -> io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if fs::symlink_metadata(path).is_ok() {
                return Err(io::Error::other("JSONL destination is a dangling alias"));
            }
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            Ok(fs::canonicalize(parent)?.join(
                path.file_name()
                    .ok_or_else(|| io::Error::other("JSONL destination has no file name"))?,
            ))
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn sibling(path: &Path, suffix: &str) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::other("JSONL destination has no file name"))?
        .to_os_string();
    name.push(suffix);
    Ok(path.with_file_name(name))
}

/// Compare resolved identities conservatively before destructive retention.
/// Windows directories may be case-sensitive; rejecting a case-only alias
/// there is safer than overwriting a configured file on normal Windows paths.
pub(crate) fn same_identity(left: &Path, right: &Path) -> io::Result<bool> {
    #[cfg(not(windows))]
    {
        Ok(left == right)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Globalization::{
            CompareStringOrdinal, CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN,
        };
        if left.as_os_str().is_empty() || right.as_os_str().is_empty() {
            return Ok(left == right);
        }
        let left: Vec<u16> = left.as_os_str().encode_wide().collect();
        let right: Vec<u16> = right.as_os_str().encode_wide().collect();
        let count = |length: usize| {
            i32::try_from(length)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "JSONL path is too long"))
        };
        // SAFETY: both vectors stay alive; explicit lengths fit i32 and bound
        // every UTF-16 read. 1 is the documented TRUE value for this API.
        match unsafe {
            CompareStringOrdinal(
                left.as_ptr(),
                count(left.len())?,
                right.as_ptr(),
                count(right.len())?,
                1,
            )
        } {
            CSTR_EQUAL => Ok(true),
            CSTR_LESS_THAN | CSTR_GREATER_THAN => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

pub(crate) fn acquire(path: &Path) -> io::Result<LockedFile> {
    acquire_for(path, Duration::from_secs(1))
}

fn acquire_for(path: &Path, timeout: Duration) -> io::Result<LockedFile> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let path = identity(path)?;
    let lock_path = sibling(&path, ".wd-write.lock")?;
    // Empty lock files contain no user data. Reject already redirected locks.
    if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(io::Error::other("JSONL lock is not a regular file"));
        }
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    let deadline = Instant::now() + timeout;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(LockedFile { path, _lock: lock }),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "JSONL writer is busy; no row was changed",
                ))
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
}

pub(crate) fn append_locked(path: &Path, line: &[u8]) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line)
}

#[cfg(test)]
#[path = "jsonl_file_tests.rs"]
mod tests;
