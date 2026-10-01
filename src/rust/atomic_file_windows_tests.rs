use crate::atomic_file::write_private;
use std::fs;

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
