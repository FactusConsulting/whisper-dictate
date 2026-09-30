//! Same-directory replacement for durable user JSON, never delete-then-rename.
//!
//! Contents are completely written and synced before publication. On Unix the
//! containing directory is synced before success is returned. A directory-sync
//! error after publication means durability is uncertain, not a rollback. This
//! does not promise power-loss protection on every filesystem or lost-update
//! protection for concurrent read/modify/write callers.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub(crate) enum Permissions {
    Preserve,
    Private,
}

pub(crate) fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_with(path, Permissions::Preserve, |file| file.write_all(contents))
}

pub(crate) fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_with(path, Permissions::Private, |file| file.write_all(contents))
}

fn write_with(
    requested_path: &Path,
    permissions: Permissions,
    write_contents: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    write_with_replace(requested_path, permissions, write_contents, |from, to| {
        fs::rename(from, to)
    })
}

fn write_with_replace(
    requested_path: &Path,
    permissions: Permissions,
    write_contents: impl FnOnce(&mut File) -> io::Result<()>,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    // Continue writing through a user's symlink, rather than replacing it.
    let destination = match fs::symlink_metadata(requested_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => fs::canonicalize(requested_path)?,
        Ok(_) => requested_path.to_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => requested_path.to_owned(),
        Err(error) => return Err(error),
    };
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let previous = match fs::metadata(&destination) {
        Ok(metadata) if metadata.is_file() => Some(metadata),
        Ok(_) => return Err(io::Error::other("atomic-write destination is not a file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let (temporary, mut file) = create_temp(parent, permissions)?;
    let cleanup = Cleanup(temporary);
    #[cfg(windows)]
    if previous.is_some() {
        windows_permissions::copy_dacl(&destination, &cleanup.0)?;
    }
    #[cfg(unix)]
    if let Some(previous) = previous
        .as_ref()
        .filter(|_| matches!(permissions, Permissions::Preserve))
    {
        file.set_permissions(previous.permissions())?;
    }
    #[cfg(not(unix))]
    let _ = permissions;
    write_contents(&mut file)?;
    file.flush()?;
    file.sync_all()?;
    drop(file); // No open temporary handle blocks replacement on Windows.
                // Existing read-only files must not become writable as a side effect.
    #[cfg(windows)]
    if let Some(previous) = previous {
        fs::set_permissions(&cleanup.0, previous.permissions())?;
    }
    replace(&cleanup.0, &destination)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn create_temp(parent: &Path, permissions: Permissions) -> io::Result<(PathBuf, File)> {
    for _ in 0..64 {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".wd-write-{}-{sequence}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if matches!(permissions, Permissions::Private) {
                0o600
            } else {
                0o666
            });
        }
        #[cfg(not(unix))]
        let _ = permissions;
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve atomic-write temporary file",
    ))
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        // Remove only the uniquely created sibling, never the destination.
        #[cfg(windows)]
        if let Ok(metadata) = fs::metadata(&self.0) {
            let mut permissions = metadata.permissions();
            if permissions.readonly() {
                permissions.set_readonly(false);
                let _ = fs::set_permissions(&self.0, permissions);
            }
        }
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(windows)]
#[path = "atomic_file_windows.rs"]
mod windows_permissions;

#[cfg(test)]
#[path = "atomic_file_tests.rs"]
mod tests;
