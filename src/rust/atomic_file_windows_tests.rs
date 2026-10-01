use crate::atomic_file::write_private;
use std::fs;

#[test]
fn cleanup_removes_only_its_owned_readonly_temporary() {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_READONLY;
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("config.json");
    let temporary = directory.path().join(".wd-write-owned.tmp");
    fs::write(&destination, b"last good").unwrap();
    fs::write(&temporary, b"incomplete").unwrap();
    let original_attributes = fs::metadata(&destination).unwrap().file_attributes();
    for path in [&destination, &temporary] {
        let attributes = fs::metadata(path).unwrap().file_attributes();
        super::copy_attributes(path, attributes | FILE_ATTRIBUTE_READONLY).unwrap();
    }
    let cleanup = crate::atomic_file::Cleanup(temporary.clone());
    drop(cleanup);
    assert!(!temporary.exists(), "owned read-only temporary was abandoned");
    assert_eq!(fs::read(&destination).unwrap(), b"last good");
    assert_eq!(
        fs::metadata(&destination).unwrap().file_attributes(),
        original_attributes | FILE_ATTRIBUTE_READONLY
    );
    // Release only this fixture's flag so TempDir can clean up on Windows.
    super::copy_attributes(&destination, original_attributes).unwrap();
}

#[test]
fn hidden_system_and_indexing_attributes_survive_replacement() {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_SYSTEM,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hidden keys.json");
    fs::write(&path, b"synthetic").unwrap();
    let flags = FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED;
    super::copy_attributes(
        &path,
        fs::metadata(&path).unwrap().file_attributes() | flags,
    )
    .unwrap();
    let before = fs::metadata(&path).unwrap().file_attributes();
    // Inspect the owned replacement before publication as well as the final file.
    crate::atomic_file::write_with_replace(
        &path,
        crate::atomic_file::Permissions::Private,
        |file| {
            use std::io::Write;
            file.write_all(b"next synthetic")
        },
        |temporary, destination| {
            assert_eq!(fs::metadata(temporary)?.file_attributes(), before);
            fs::rename(temporary, destination)
        },
    )
    .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().file_attributes(), before);
    assert_eq!(fs::read(&path).unwrap(), b"next synthetic");
    assert_no_temporary_files(directory.path());
}

#[test]
fn unsupported_file_attributes_fail_closed_instead_of_being_stripped() {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_SPARSE_FILE,
    };
    for unsupported in [
        FILE_ATTRIBUTE_COMPRESSED,
        FILE_ATTRIBUTE_SPARSE_FILE,
        1 << 31,
    ] {
        assert_eq!(
            super::validate_attribute_inheritance(
                FILE_ATTRIBUTE_ARCHIVE | unsupported,
                FILE_ATTRIBUTE_ARCHIVE
            )
            .unwrap_err()
            .kind(),
            std::io::ErrorKind::Unsupported
        );
    }
    assert!(super::validate_attribute_inheritance(
        FILE_ATTRIBUTE_ARCHIVE | FILE_ATTRIBUTE_COMPRESSED,
        FILE_ATTRIBUTE_ARCHIVE | FILE_ATTRIBUTE_COMPRESSED
    )
    .is_ok());
}

#[test]
fn replacement_copies_a_different_owner_without_reassigning_an_identical_owner() {
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        SE_DACL_PROTECTED, UNPROTECTED_DACL_SECURITY_INFORMATION,
    };
    for (control, protection) in [
        (0, UNPROTECTED_DACL_SECURITY_INFORMATION),
        (SE_DACL_PROTECTED, PROTECTED_DACL_SECURITY_INFORMATION),
    ] {
        assert_eq!(
            super::replacement_security_information(control, true),
            DACL_SECURITY_INFORMATION | protection
        );
        assert_eq!(
            super::replacement_security_information(control, false),
            DACL_SECURITY_INFORMATION | protection | OWNER_SECURITY_INFORMATION
        );
    }
}

#[test]
fn efs_encryption_is_rejected_even_when_other_attributes_are_present() {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_ENCRYPTED,
    };
    assert!(super::reject_encrypted(FILE_ATTRIBUTE_ARCHIVE).is_ok());
    for attributes in [
        FILE_ATTRIBUTE_ENCRYPTED,
        FILE_ATTRIBUTE_ENCRYPTED | FILE_ATTRIBUTE_ARCHIVE,
    ] {
        let error = super::reject_encrypted(attributes).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("EFS-encrypted"));
    }
}

fn assert_no_temporary_files(path: &std::path::Path) {
    assert!(fs::read_dir(path).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".wd-write-")));
}

#[test]
fn windows_atomic_replacement_preserves_an_explicit_protected_acl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys with spaces.json");
    fs::write(&path, "{}").unwrap();
    let before = powershell(
        &path,
        r#"
        $ErrorActionPreference = 'Stop'
        $acl = New-Object System.Security.AccessControl.FileSecurity
        $acl.SetAccessRuleProtection($true, $false)
        $sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
        $rule = New-Object System.Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'Allow')
        $acl.AddAccessRule($rule)
        [System.IO.File]::SetAccessControl($env:WD_ATOMIC_TEST_PATH, $acl)
        [System.IO.File]::GetAccessControl($env:WD_ATOMIC_TEST_PATH).Sddl
    "#,
    );
    write_private(&path, br#"{"test":"synthetic credential"}"#).unwrap();
    let after = powershell(
        &path,
        "[System.IO.File]::GetAccessControl($env:WD_ATOMIC_TEST_PATH).Sddl",
    );
    assert_eq!(before, after);
    assert_no_temporary_files(dir.path());
}

fn powershell(path: &std::path::Path, script: &str) -> String {
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("WD_ATOMIC_TEST_PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn windows_atomic_replacement_preserves_an_unprotected_inherited_acl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inherited keys.json");
    fs::write(&path, "{}").unwrap();
    let before = powershell(
        &path,
        r#"
        $ErrorActionPreference = 'Stop'
        $acl = [System.IO.File]::GetAccessControl($env:WD_ATOMIC_TEST_PATH)
        $acl.SetAccessRuleProtection($false, $true)
        $sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
        $rule = New-Object System.Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'Allow')
        $acl.AddAccessRule($rule)
        [System.IO.File]::SetAccessControl($env:WD_ATOMIC_TEST_PATH, $acl)
        $saved = [System.IO.File]::GetAccessControl($env:WD_ATOMIC_TEST_PATH)
        if ($saved.AreAccessRulesProtected) { throw 'ACL unexpectedly protected' }
        if (-not @($saved.Access | Where-Object { $_.IsInherited }).Count) { throw 'No inherited ACEs' }
        $saved.Sddl
    "#,
    );
    write_private(&path, br#"{"test":"synthetic credential"}"#).unwrap();
    let after = powershell(
        &path,
        r#"
        $saved = [System.IO.File]::GetAccessControl($env:WD_ATOMIC_TEST_PATH)
        if ($saved.AreAccessRulesProtected) { throw 'ACL became protected' }
        $saved.Sddl
    "#,
    );
    assert_eq!(before, after);
    assert_no_temporary_files(dir.path());
}
