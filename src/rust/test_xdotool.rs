//! Isolated fake helper; never talks to the desktop or leaves PATH changed.
use std::os::unix::fs::PermissionsExt;

struct RestorePath(Option<std::ffi::OsString>);
impl Drop for RestorePath {
    fn drop(&mut self) {
        match &self.0 {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

pub(crate) fn with_script(script: &str, run: impl FnOnce()) {
    let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("xdotool");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _restore = RestorePath(std::env::var_os("PATH"));
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", dir.path().display()));
    run();
}
