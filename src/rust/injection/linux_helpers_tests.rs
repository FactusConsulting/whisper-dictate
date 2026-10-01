use super::{invoke_type, invoke_type_cancellable, release_modifiers_best_effort_cancellable};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

struct RestorePath(Option<std::ffi::OsString>);
impl Drop for RestorePath {
    fn drop(&mut self) {
        match &self.0 {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

fn with_helper(helper: &str, script: &str, run: impl FnOnce()) {
    let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(helper);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let _restore = RestorePath(std::env::var_os("PATH"));
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", dir.path().display()));
    run();
}

#[test]
fn helper_fixture_is_owner_only() {
    with_helper("fixture", "exit 0", || {});
}

#[test]
fn dotool_cancellation_covers_blocked_stdin_and_does_not_try_another_helper() {
    with_helper("dotool", "sleep 2", || {
        let started = Instant::now();
        let error = invoke_type_cancellable("dotool", &"x".repeat(4 * 1024 * 1024), &|| {
            started.elapsed() < Duration::from_millis(300)
        })
        .unwrap_err();
        assert!(error.to_string().contains("injection cancelled"));
        assert!(started.elapsed() < Duration::from_millis(1500));
    });
}

#[test]
fn dotool_diagnostics_can_exceed_pipe_capacity_without_stalling_input() {
    with_helper(
        "dotool",
        "head -c 262144 /dev/zero >&2\ncat >/dev/null",
        || {
            invoke_type("dotool", "first line\nsecond line").unwrap();
        },
    );
}

#[test]
fn cancellation_interrupts_a_stalled_modifier_release() {
    with_helper("ydotool", "sleep 2", || {
        let started = Instant::now();
        let error = release_modifiers_best_effort_cancellable(
            |helper| (helper == "ydotool").then(|| std::path::PathBuf::from(helper)),
            &|| started.elapsed() < Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(error.to_string().contains("injection cancelled"));
        assert!(started.elapsed() < Duration::from_millis(1500));
    });
}
