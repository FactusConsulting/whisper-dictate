//! File-relationship guards and mandatory integrity-label preservation.
//!
//! LABEL_SECURITY_INFORMATION is available with READ_CONTROL/WRITE_OWNER.
//! Full audit SACL access needs SeSecurityPrivilege; this unprivileged desktop
//! writer does not promise custom file-auditing preservation. See CONFIGURATION.

use std::fs::OpenOptions;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{
    ERROR_HANDLE_EOF, ERROR_INVALID_PARAMETER, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{GetSecurityDescriptorSacl, ACL, LABEL_SECURITY_INFORMATION};
use windows_sys::Win32::Storage::FileSystem::{
    FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_READ_ATTRIBUTES,
    WIN32_FIND_STREAM_DATA,
};

pub(super) fn validate_destination(path: &Path) -> io::Result<()> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .open(path)?;
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: file owns the handle; information is a writable SDK structure.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if information.nNumberOfLinks > 1 {
        return Err(unsupported("multiply linked files"));
    }
    reject_named_streams(path)
}

fn reject_named_streams(path: &Path) -> io::Result<()> {
    let path = super::wide_path(path)?;
    let mut data = WIN32_FIND_STREAM_DATA::default();
    // SAFETY: path is terminated and data has the requested SDK layout.
    let handle = unsafe {
        FindFirstStreamW(
            path.as_ptr(),
            FindStreamInfoStandard,
            (&mut data as *mut WIN32_FIND_STREAM_DATA).cast(),
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        return if matches!(error.raw_os_error(), Some(code) if code == ERROR_HANDLE_EOF as i32 || code == ERROR_INVALID_PARAMETER as i32)
        {
            // No streams, or a filesystem that does not implement streams.
            Ok(())
        } else {
            Err(error)
        };
    }
    let search = StreamSearch(handle);
    let unnamed: Vec<_> = "::$DATA".encode_utf16().collect();
    loop {
        let end = data
            .cStreamName
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(data.cStreamName.len());
        if data.cStreamName[..end] != unnamed {
            return Err(unsupported("NTFS named streams"));
        }
        // SAFETY: search owns this valid handle; data remains writable.
        if unsafe { FindNextStreamW(search.0, (&mut data as *mut WIN32_FIND_STREAM_DATA).cast()) }
            == 0
        {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
                Ok(())
            } else {
                Err(error)
            };
        }
    }
}

pub(super) fn copy_label(from: &Path, to: &Path) -> io::Result<()> {
    let (source, label) = label_descriptor(from)?;
    let (temporary, current_label) = label_descriptor(to)?;
    if label_bytes(label) == label_bytes(current_label) {
        return Ok(());
    }
    let path = super::wide_path(to)?;
    // SAFETY: label remains owned by source; path is valid and terminated.
    let error = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            label,
        )
    };
    drop((source, temporary));
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error as i32))
    }
}

fn label_descriptor(path: &Path) -> io::Result<(super::Descriptor, *mut ACL)> {
    let path = super::wide_path(path)?;
    let mut descriptor = null_mut();
    // SAFETY: path and output pointer are valid; LocalFree owns the result.
    let error = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let allocation = super::Descriptor(descriptor);
    let (mut present, mut defaulted) = (0, 0);
    let mut label = null_mut();
    // SAFETY: allocation holds the valid descriptor returned by the SDK.
    if unsafe { GetSecurityDescriptorSacl(allocation.0, &mut present, &mut label, &mut defaulted) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((allocation, if present == 0 { null_mut() } else { label }))
}

fn label_bytes(label: *const ACL) -> Vec<u8> {
    if label.is_null() {
        return Vec::new();
    }
    // SAFETY: only called with an ACL in a live descriptor returned by the SDK.
    unsafe { std::slice::from_raw_parts(label.cast::<u8>(), (*label).AclSize as usize).to_vec() }
}

fn unsupported(kind: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("atomic replacement of {kind} is not supported; existing file was preserved"),
    )
}

struct StreamSearch(windows_sys::Win32::Foundation::HANDLE);
impl Drop for StreamSearch {
    fn drop(&mut self) {
        // SAFETY: this is the uniquely owned stream-search handle.
        unsafe {
            FindClose(self.0);
        }
    }
}

#[cfg(test)]
#[path = "atomic_file_windows_metadata_tests.rs"]
mod tests;
