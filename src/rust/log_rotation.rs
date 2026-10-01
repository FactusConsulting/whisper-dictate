//! Bounded best-effort GUI diagnostics; never rotate configured user data.
//! No persistent data handle: reopen after taking the cooperative sidecar lock
//! so concurrent GUI writers cannot append to an obsolete file on Windows.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{atomic_file, config, jsonl_file};

const LOG_BYTES: usize = 4 * 1024 * 1024;
const RECORD_BYTES: usize = 1024 * 1024;
type RotationGuard = fn(&Path, &Path) -> io::Result<()>;

pub(crate) struct RotatingLog {
    path: PathBuf,
    limit: usize,
    pending: Vec<u8>,
    discarded: bool,
    guard: RotationGuard,
}

impl RotatingLog {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        Self::open_with(path, LOG_BYTES, protect_user_data)
    }

    fn open_with(path: &Path, limit: usize, guard: RotationGuard) -> io::Result<Self> {
        if limit == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero log limit",
            ));
        }
        let locked = jsonl_file::acquire(path)?;
        drop(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&locked.path)?,
        );
        Ok(Self {
            path: path.to_owned(),
            limit,
            pending: Vec::new(),
            discarded: false,
            guard,
        })
    }

    fn publish(&self, record: &[u8]) -> io::Result<()> {
        let locked = jsonl_file::acquire(&self.path)?;
        let length = fs::metadata(&locked.path)?.len();
        if length.saturating_add(record.len() as u64) <= self.limit as u64 {
            return jsonl_file::append_locked(&locked.path, record);
        }
        let backup = jsonl_file::sibling(&locked.path, ".previous")?;
        (self.guard)(&locked.path, &backup)?;
        match fs::symlink_metadata(&backup) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(io::Error::other("diagnostic backup is not a regular file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // Publish recovery first. Failed replacement preserves the active log
        // and recovery; do not move/delete the active log before publication.
        atomic_file::copy_private_tail(&locked.path, &backup, self.limit as u64)?;
        atomic_file::write_stream(&locked.path, |out| out.write_all(record))
    }
}

impl Write for RotatingLog {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.discarded
            || self.pending.len().saturating_add(bytes.len()) > RECORD_BYTES.min(self.limit)
        {
            self.pending.clear();
            self.discarded = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "diagnostic record exceeds safety limit",
            ));
        }
        self.pending.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if std::mem::take(&mut self.discarded) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized diagnostic record discarded",
            ));
        }
        if self.pending.is_empty() {
            return Ok(());
        }
        // Consume even on I/O error. A later record must not retry or duplicate
        // a partially appended previous one; stderr remains a separate sink.
        let record = std::mem::take(&mut self.pending);
        self.publish(&record)
    }
}

fn protect_user_data(active: &Path, backup: &Path) -> io::Result<()> {
    let raw = config::load_raw_config()
        .map_err(|_| io::Error::other("diagnostic rotation cannot verify config"))?;
    if !raw.is_object() {
        return Err(io::Error::other(
            "diagnostic rotation requires an object config",
        ));
    }
    let effective = config::effective_runtime_config_from_raw(&raw);
    let mut paths = vec![config::config_path(), config::default_history_path()];
    for key in ["history_jsonl", "metrics_jsonl"] {
        if let Some(value) = effective
            .get(key)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            paths.push(crate::dictate::session::path_util::expand_user(value));
        }
    }
    let history_paths: Vec<_> = paths
        .iter()
        .skip(1)
        .map(|p| jsonl_file::sibling(p, ".retention-backup"))
        .collect::<io::Result<_>>()?;
    paths.extend(history_paths);
    for path in paths {
        match jsonl_file::identity(&path) {
            Ok(path) if same_path(&path, active) || same_path(&path, backup) => {
                return Err(io::Error::other(
                    "diagnostic rotation would replace configured user data",
                ));
            }
            Ok(_) => {}
            // A nonexistent parent cannot alias the existing active/backup
            // parent. Missing files in that parent still get identity checks.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    // Existing identities are canonical. A missing .previous file has the
    // same canonical parent but Windows still folds its ASCII basename.
    #[cfg(windows)]
    return left
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy());
    #[cfg(not(windows))]
    return left == right;
}

#[cfg(test)]
#[path = "log_rotation_policy_tests.rs"]
mod policy_tests;
#[cfg(test)]
#[path = "log_rotation_tests.rs"]
mod tests;
