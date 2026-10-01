//! Preserve Unix ownership and extended security metadata before writing bytes.

use std::fs::{File, Metadata};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub(super) fn copy_security(from: &Path, to: &File, previous: &Metadata) -> io::Result<()> {
    let temporary = to.metadata()?;
    if previous.uid() != temporary.uid() || previous.gid() != temporary.gid() {
        // SAFETY: to owns this descriptor; uid/gid came from the destination.
        if unsafe { libc::fchown(to.as_raw_fd(), previous.uid(), previous.gid()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    copy_extended_attributes(from, to)
}

#[cfg(target_os = "linux")]
fn copy_extended_attributes(from: &Path, to: &File) -> io::Result<()> {
    // Opening for write preserves compatibility with a write-only destination.
    // Attribute access still performs its own kernel permission checks.
    let source = std::fs::OpenOptions::new().write(true).open(from)?;
    let source_fd = source.as_raw_fd();
    let destination_fd = to.as_raw_fd();
    let expected = attribute_names(source_fd)?;
    for inherited in attribute_names(destination_fd)? {
        if !expected.contains(&inherited) {
            // SAFETY: descriptor and NUL-terminated attribute name are valid.
            if unsafe { libc::fremovexattr(destination_fd, inherited.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    for name in expected {
        let mut value = vec![0u8; 65_536];
        // SAFETY: source/name are valid; value owns its writable buffer.
        let size = unsafe {
            libc::fgetxattr(
                source_fd,
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        if size < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: use only the initialized bytes returned by fgetxattr.
        if unsafe {
            libc::fsetxattr(
                destination_fd,
                name.as_ptr(),
                value.as_ptr().cast(),
                size as usize,
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn attribute_names(fd: std::os::fd::RawFd) -> io::Result<Vec<std::ffi::CString>> {
    // Linux bounds the complete name list to 64 KiB. One read avoids a
    // query/read size race; oversized or unreadable metadata fails closed.
    let mut names = vec![0u8; 65_536];
    // SAFETY: the live file descriptor and writable buffer are valid.
    let size = unsafe { libc::flistxattr(fd, names.as_mut_ptr().cast(), names.len()) };
    if size < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOTSUP) {
            Ok(Vec::new())
        } else {
            Err(error)
        };
    }
    names[..size as usize]
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| {
            std::ffi::CString::new(name)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid attribute name"))
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn copy_extended_attributes(_from: &Path, _to: &File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic replacement security metadata is supported on Linux and Windows; existing file was preserved",
    ))
}

#[cfg(all(test, target_os = "linux"))]
#[path = "atomic_file_unix_tests.rs"]
mod tests;
