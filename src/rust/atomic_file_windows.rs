//! Preserve a destination's discretionary ACL before putting any data in its
//! replacement. Native rename alone inherits the temporary's parent ACL.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetSecurityDescriptorControl, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED, UNPROTECTED_DACL_SECURITY_INFORMATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    SetFileAttributesW, FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_HIDDEN,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_SYSTEM, FILE_ATTRIBUTE_TEMPORARY,
};

const SUPPORTED_ATTRIBUTES: u32 = FILE_ATTRIBUTE_ARCHIVE
    | FILE_ATTRIBUTE_HIDDEN
    | FILE_ATTRIBUTE_NORMAL
    | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
    | FILE_ATTRIBUTE_READONLY
    | FILE_ATTRIBUTE_SYSTEM
    | FILE_ATTRIBUTE_TEMPORARY;

pub(super) fn validate_attributes(attributes: u32) -> io::Result<()> {
    reject_encrypted(attributes)
}

pub(super) fn validate_destination(path: &Path) -> io::Result<()> {
    metadata::validate_destination(path)
}

fn validate_attribute_inheritance(previous: u32, temporary: u32) -> io::Result<()> {
    validate_attributes(previous)?;
    if (previous ^ temporary) & !SUPPORTED_ATTRIBUTES != 0 {
        return Err(io::Error::new(io::ErrorKind::Unsupported,
            "atomic replacement cannot preserve these Windows file attributes; existing file was preserved"));
    }
    Ok(())
}

pub(super) fn copy_attributes(path: &Path, attributes: u32) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    validate_attribute_inheritance(attributes, std::fs::metadata(path)?.file_attributes())?;
    let path = wide_path(path)?;
    let settable = attributes & SUPPORTED_ATTRIBUTES;
    let settable = if settable == 0 {
        FILE_ATTRIBUTE_NORMAL
    } else {
        settable
    };
    // SAFETY: path is terminated UTF-16; attributes are validated settable flags.
    if unsafe { SetFileAttributesW(path.as_ptr(), settable) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn reject_encrypted(attributes: u32) -> io::Result<()> {
    if attributes & FILE_ATTRIBUTE_ENCRYPTED != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "atomic replacement of an EFS-encrypted file is not supported; existing file was preserved",
        ));
    }
    Ok(())
}

pub(super) fn copy_dacl(from: &Path, to: &Path) -> io::Result<()> {
    metadata::copy_label(from, to)?;
    let from = wide_path(from)?;
    let to = wide_path(to)?;
    let mut dacl = null_mut();
    let mut owner = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: both paths are NUL-terminated UTF-16; the out pointers are valid.
    // The returned ACL is owned by descriptor and freed after SetNamedSecurityInfoW.
    let error = unsafe {
        GetNamedSecurityInfoW(
            from.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let allocation = Descriptor(descriptor);
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: GetNamedSecurityInfoW returned this valid descriptor.
    if unsafe { GetSecurityDescriptorControl(allocation.0, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut temporary_owner = null_mut();
    let mut temporary_descriptor = null_mut();
    // SAFETY: the path is terminated and both out pointers are valid. The owner
    // lives in the returned descriptor until its RAII allocation is dropped.
    let error = unsafe {
        GetNamedSecurityInfoW(
            to.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut temporary_owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut temporary_descriptor,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let _temporary_allocation = Descriptor(temporary_descriptor);
    if owner.is_null() || temporary_owner.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file owner is unavailable; existing file was preserved",
        ));
    }
    // SAFETY: both descriptors returned valid non-null owner SIDs.
    let owner_is_same = unsafe { EqualSid(owner, temporary_owner) } != 0;
    let security_information = replacement_security_information(control, owner_is_same);
    // Copy a different owner together with the ACL, or fail before writing any
    // replacement contents if the required WRITE_OWNER/privilege is unavailable.
    // An unchanged owner needs no extra permission or ownership operation.
    let replacement_owner = if owner_is_same { null_mut() } else { owner };
    // SAFETY: the ACL/SID remain valid until allocation drops; to is a valid path.
    let error = unsafe {
        SetNamedSecurityInfoW(
            to.as_ptr(),
            SE_FILE_OBJECT,
            security_information,
            replacement_owner,
            null_mut(),
            dacl,
            null(),
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    Ok(())
}

fn replacement_security_information(control: u16, owner_is_same: bool) -> u32 {
    let protection = if control & SE_DACL_PROTECTED != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    DACL_SECURITY_INFORMATION
        | protection
        | if owner_is_same {
            0
        } else {
            OWNER_SECURITY_INFORMATION
        }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    // Both files already exist. Windows canonicalize supplies the extended
    // path prefix, keeping long paths compatible with the filesystem APIs.
    let absolute = std::fs::canonicalize(path)?;
    let mut wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

struct Descriptor(windows_sys::Win32::Security::PSECURITY_DESCRIPTOR);

#[path = "atomic_file_windows_metadata.rs"]
mod metadata;

#[cfg(test)]
#[path = "atomic_file_windows_tests.rs"]
mod tests;

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: GetNamedSecurityInfoW allocates with LocalAlloc.
        unsafe {
            LocalFree(self.0);
        }
    }
}
