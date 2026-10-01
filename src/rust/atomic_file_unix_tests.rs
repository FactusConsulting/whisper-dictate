//! Real Linux ACL/extended-attribute regressions, with no external ACL tools.

use crate::atomic_file::{write, write_private};
use std::ffi::CString;
use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

#[test]
fn identical_inherited_label_does_not_require_relabel_permission() {
    let label = b"confined_u:object_r:config_t:s0";
    super::apply_attribute_if_changed(label, Some(label), |_| {
        Err(std::io::Error::from_raw_os_error(libc::EPERM))
    })
    .expect("an already correct label must not attempt the forbidden relabel");
}

#[test]
fn absent_or_different_protected_label_still_fails_closed_without_permission() {
    for current in [None, Some(b"wrong label".as_slice())] {
        let error = super::apply_attribute_if_changed(b"expected label", current, |_| {
            Err(std::io::Error::from_raw_os_error(libc::EPERM))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    }
}

fn set_attribute(path: &Path, name: &str, value: &[u8]) {
    let file = File::open(path).unwrap();
    let name = CString::new(name).unwrap();
    // SAFETY: live descriptor and both buffers remain valid for this call.
    let result = unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
}

fn attribute(path: &Path, name: &str) -> Option<Vec<u8>> {
    let file = File::open(path).unwrap();
    let name = CString::new(name).unwrap();
    let mut bytes = vec![0u8; 65_536];
    // SAFETY: live descriptor, terminated name and writable buffer are valid.
    let size = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if size < 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENODATA)
        );
        None
    } else {
        bytes.truncate(size as usize);
        Some(bytes)
    }
}

fn shared_acl() -> Vec<u8> {
    // Linux POSIX ACL xattr v2: owner rw, named nobody r, owning group none,
    // mask r, other none. The extra account is a fixture, not a host mutation.
    let mut value = 2u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1u16, 6u16, u32::MAX),
        (2, 4, 65_534),
        (4, 0, u32::MAX),
        (16, 4, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        value.extend(tag.to_le_bytes());
        value.extend(permissions.to_le_bytes());
        value.extend(id.to_le_bytes());
    }
    value
}

#[test]
fn replacement_preserves_shared_acl_owner_group_and_extended_attributes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared.json");
    fs::write(&path, b"last good").unwrap();
    // When the unprivileged CI account has another group, make the source
    // group-shared without changing the parent's default group. This exercises
    // the actual fchown path as well as ACL preservation.
    // SAFETY: a zero-sized query requires no output buffer.
    let group_count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    assert!(group_count >= 0);
    let mut groups = vec![0 as libc::gid_t; group_count as usize];
    // SAFETY: groups owns room for exactly group_count entries.
    assert!(unsafe { libc::getgroups(group_count, groups.as_mut_ptr()) } >= 0);
    let source = File::open(&path).unwrap();
    let original = source.metadata().unwrap();
    if let Some(group) = groups.into_iter().find(|group| *group != original.gid()) {
        // SAFETY: valid descriptor; this group belongs to the current process.
        assert_eq!(
            unsafe { libc::fchown(source.as_raw_fd(), original.uid(), group) },
            0
        );
    }
    set_attribute(&path, "system.posix_acl_access", &shared_acl());
    set_attribute(&path, "user.wd-fixture", b"metadata");
    let before = fs::metadata(&path).unwrap();
    let acl = attribute(&path, "system.posix_acl_access").unwrap();
    write(&path, b"next").unwrap();
    let after = fs::metadata(&path).unwrap();
    assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
    assert_eq!(after.permissions().mode(), before.permissions().mode());
    assert_eq!(attribute(&path, "system.posix_acl_access"), Some(acl));
    assert_eq!(
        attribute(&path, "user.wd-fixture"),
        Some(b"metadata".to_vec())
    );
    assert_eq!(fs::read(&path).unwrap(), b"next");
}

#[test]
fn parent_default_acl_does_not_add_access_to_an_existing_plain_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    fs::write(&path, b"last good").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(attribute(&path, "system.posix_acl_access").is_none());
    set_attribute(directory.path(), "system.posix_acl_default", &shared_acl());
    write(&path, b"next").unwrap();
    assert!(attribute(&path, "system.posix_acl_access").is_none());
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(fs::read(&path).unwrap(), b"next");
}

#[test]
fn private_replacement_preserves_acl_entries_but_revokes_the_shared_mask() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keys.json");
    fs::write(&path, b"synthetic").unwrap();
    set_attribute(&path, "system.posix_acl_access", &shared_acl());
    write_private(&path, b"next synthetic").unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let acl = attribute(&path, "system.posix_acl_access").unwrap();
    assert_eq!(&acl[28..30], &16u16.to_le_bytes());
    assert_eq!(&acl[30..32], &0u16.to_le_bytes());
    assert_eq!(fs::read(&path).unwrap(), b"next synthetic");
}
