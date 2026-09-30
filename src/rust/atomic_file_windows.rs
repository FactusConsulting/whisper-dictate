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
    GetSecurityDescriptorControl, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    SE_DACL_PROTECTED, UNPROTECTED_DACL_SECURITY_INFORMATION,
};

pub(super) fn copy_dacl(from: &Path, to: &Path) -> io::Result<()> {
    let from = wide_path(from)?;
    let to = wide_path(to)?;
    let mut dacl = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: both paths are NUL-terminated UTF-16; the out pointers are valid.
    // The returned ACL is owned by descriptor and freed after SetNamedSecurityInfoW.
    let error = unsafe {
        GetNamedSecurityInfoW(
            from.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
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
    let protection = if control & SE_DACL_PROTECTED != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    // SAFETY: the ACL remains valid until allocation drops; to is a valid path.
    let error = unsafe {
        SetNamedSecurityInfoW(
            to.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | protection,
            null_mut(),
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
