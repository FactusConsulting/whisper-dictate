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

pub(crate) fn write_stream(
    path: &Path,
    contents: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    write_with(path, Permissions::Preserve, contents)
}

/// A streaming recovery copy inherits the source's security before any bytes
/// are written; Unix backups are additionally restricted to owner access.
pub(crate) fn copy_private(source: &Path, destination: &Path) -> io::Result<()> {
    copy_private_with(source, destination, |out| {
        io::copy(&mut File::open(source)?, out)?;
        Ok(())
    })
}

/// Bound diagnostic recovery to a whole-line tail without loading the old log.
pub(crate) fn copy_private_tail(source: &Path, destination: &Path, budget: u64) -> io::Result<()> {
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
    copy_private_with(source, destination, |out| {
        let mut input = File::open(source)?;
        let start = input.metadata()?.len().saturating_sub(budget);
        let mut partial = false;
        if start > 0 {
            input.seek(SeekFrom::Start(start - 1))?;
            let mut previous = [0];
            input.read_exact(&mut previous)?;
            partial = previous[0] != b'\n';
        }
        let mut reader = BufReader::new(input);
        while partial {
            let bytes = reader.fill_buf()?;
            if bytes.is_empty() {
                break;
            }
            let newline = bytes.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(bytes.len(), |index| index + 1);
            reader.consume(consumed);
            partial = newline.is_none();
        }
        io::copy(&mut reader.take(budget), out)?;
        Ok(())
    })
}

fn copy_private_with(
    source: &Path,
    destination: &Path,
    contents: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    write_with_metadata(
        destination,
        Some(source),
        Permissions::Private,
        contents,
        |from, to| fs::rename(from, to),
    )
}

/// Remove the resolved file and persist that namespace change on Unix.
/// A sync failure is reported after removal, not as a rollback.
pub(crate) fn remove(path: &Path) -> io::Result<()> {
    remove_with_sync(path, |parent| {
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        #[cfg(not(unix))]
        let _ = parent;
        Ok(())
    })
}

fn remove_with_sync(
    path: &Path,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let destination = resolve_destination(path)?;
    fs::remove_file(&destination)?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    sync_parent(parent)
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
    write_with_metadata(requested_path, None, permissions, write_contents, replace)
}

fn write_with_metadata(
    requested_path: &Path,
    metadata_source: Option<&Path>,
    permissions: Permissions,
    write_contents: impl FnOnce(&mut File) -> io::Result<()>,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    // Continue writing through a user's symlink, rather than replacing it.
    let destination = resolve_destination(requested_path)?;
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
    if let Some(previous) = &previous {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if previous.nlink() > 1 {
                return Err(io::Error::new(io::ErrorKind::Unsupported,
                    "atomic replacement of a multiply linked file is not supported; existing links were preserved"));
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            windows_permissions::validate_attributes(previous.file_attributes())?;
            windows_permissions::validate_destination(&destination)?;
        }
        // Check the original file's write permission/ACL without truncating it.
        // A writable parent must not bypass a read-only destination.
        drop(OpenOptions::new().write(true).open(&destination)?);
        let _ = previous;
    }
    let (temporary, mut file) = create_temp(parent, permissions)?;
    let cleanup = Cleanup(temporary);
    let source = metadata_source.unwrap_or(&destination);
    let source_metadata = metadata_source.map(fs::metadata).transpose()?;
    let security = source_metadata.as_ref().or(previous.as_ref());
    #[cfg(windows)]
    if security.is_some() {
        windows_permissions::copy_dacl(source, &cleanup.0)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(security) = security {
            unix_permissions::copy_security(source, &file, security)?;
        }
        if matches!(permissions, Permissions::Private) {
            // OpenOptions::mode is filtered by umask. Restore owner read/write
            // before writing secret bytes, even with a restrictive umask.
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        } else if let Some(security) = security {
            file.set_permissions(security.permissions())?;
        }
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
        use std::os::windows::fs::MetadataExt;
        windows_permissions::copy_attributes(&cleanup.0, previous.file_attributes())?;
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

fn resolve_destination(requested: &Path) -> io::Result<PathBuf> {
    let mut destination = requested.to_owned();
    for _ in 0..40 {
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = fs::read_link(&destination)?;
                destination = if target.is_absolute() {
                    target
                } else {
                    destination.parent().unwrap_or(Path::new(".")).join(target)
                };
            }
            Ok(_) => return Ok(destination),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(destination),
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "atomic-write symlink chain is too deep",
    ))
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        // Remove only the uniquely created sibling, never the destination.
        #[cfg(windows)]
        if let Ok(metadata) = fs::metadata(&self.0) {
            use std::os::windows::fs::MetadataExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_READONLY;
            if metadata.permissions().readonly() {
                // Windows-only: clear the DOS read-only bit on our disposable
                // temporary, never Unix access bits or the user's destination.
                let attributes = metadata.file_attributes() & !FILE_ATTRIBUTE_READONLY;
                let _ = windows_permissions::copy_attributes(&self.0, attributes);
            }
        }
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(windows)]
#[path = "atomic_file_windows.rs"]
mod windows_permissions;

#[cfg(unix)]
#[path = "atomic_file_unix.rs"]
mod unix_permissions;

#[cfg(test)]
#[path = "atomic_file_tests.rs"]
mod tests;
